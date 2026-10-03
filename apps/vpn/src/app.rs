//! Root widget for the VPN client.
//!
//! [`VpnApp`] is a [`NavShell`] window: the saved connections are the
//! overview list and the selected connection is the content. It seeds and
//! reads the [`Vault`] from the shared Automerge document, rebuilds the
//! shell's columns from it when what they show changes, and applies what the
//! controls ask for ([`Intent`]) to the document and the machine. Import and
//! confirmation are the shared [`ModalDialog`] in the window's top layer.
//! Throughput is ephemeral, measured telemetry that never enters the
//! document.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::control_custody::RawControlSlot;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ControlInvocation};
use ui::widget::{Column, ModalDialog, NavPane, NavShell, Widget};
use zeroize::Zeroize;

use crate::actions::{self, VPN_ACTION_TYPE};
use crate::config::{self, CommandPlan};
use crate::doc::VpnDoc;
use crate::model::{
    ConnId, ConnKind, ConnStatus, Connection, LocalProfileDetails, Vault, sample_vault,
};
use crate::net::{self, Transfer};
use crate::view::{self, DetailTab, Intent, Intents, MainAction, Notice, NoticeKind, Rates};

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

fn tunnel_import_request(kind: ConnKind) -> PickRequest {
    let (title, filter) = match kind {
        ConnKind::WireGuard => (
            "Import WireGuard tunnel",
            FileFilter::new("WireGuard (.conf)").glob("*.conf"),
        ),
        ConnKind::OpenVpn => (
            "Import OpenVPN tunnel",
            FileFilter::new("OpenVPN (.ovpn)").glob("*.ovpn"),
        ),
    };
    PickRequest::open(title, "Import")
        .thread_name("vpn-portal-import")
        .filter(filter)
        .all_files()
}

/// Deterministic fictional values used only by the hermetic visual-review
/// build. Live instances populate this map solely from the locally selected
/// provider file, never from collaboration.
fn demo_local_details(vault: &Vault) -> HashMap<ConnId, LocalProfileDetails> {
    vault
        .connections
        .iter()
        .map(|connection| {
            let details = match connection.name.as_str() {
                "Home" => LocalProfileDetails {
                    network_manager_uuid: "3f9c2a71-5b8e-4d0a-9e61-2c7f4b8a1d05".into(),
                    endpoint: "home.vandenberg.net:51820".into(),
                    address: "10.8.0.2/32".into(),
                    dns: "10.8.0.1".into(),
                    public_key: "hT3mQ9vYx2Lr8KcN5eW1zJ0aPdF6sU4gBtR7oIy3Vk8=".into(),
                    allowed_ips: "0.0.0.0/0, ::/0".into(),
                },
                "Work" => LocalProfileDetails {
                    network_manager_uuid: "8b1e47d2-0c93-4f6a-b5d8-71e2a9c34f60".into(),
                    endpoint: "vpn.northwind.io:1194 (UDP)".into(),
                    address: "172.20.14.37/24".into(),
                    dns: "172.20.0.53".into(),
                    allowed_ips: "172.20.0.0/16, 10.40.0.0/16".into(),
                    ..LocalProfileDetails::default()
                },
                "Lab" => LocalProfileDetails {
                    network_manager_uuid: "c47a0e95-2d1b-4b83-9f0e-5a6d3c21b7e4".into(),
                    endpoint: "lab.kestrel-research.org:443 (TCP)".into(),
                    address: "10.12.3.18/24".into(),
                    dns: "10.12.0.1".into(),
                    allowed_ips: "10.12.0.0/16".into(),
                    ..LocalProfileDetails::default()
                },
                _ => LocalProfileDetails {
                    network_manager_uuid: "5d2f8c6b-97a4-4e1f-8c3d-0b6e9a2f7d18".into(),
                    endpoint: "relay-fra1.meshgate.net:51820".into(),
                    address: "10.66.0.12/32".into(),
                    dns: "10.66.0.1".into(),
                    public_key: "Qm7xV2cK9pZr4LwT1yH8nB6dS0fJ3gE5aU7iO2kR9lw=".into(),
                    allowed_ips: "0.0.0.0/0, ::/0".into(),
                },
            };
            (connection.id, details)
        })
        .collect()
}

/// A deterministic minute of traffic for the hermetic visual-review build:
/// a steady download with a burst, and a quieter upload. Live instances only
/// ever plot measured counters.
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

/// What the confirm sheet is about to do once the person accepts.
#[derive(Clone, Debug)]
enum Pending {
    Connect(ConnId, CommandPlan),
    Disconnect(ConnId, CommandPlan),
    TorGlobal(bool, CommandPlan),
}

/// Which sheet is open.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Modal {
    None,
    Add,
    Confirm,
}

/// What the overview currently shows, so it is rebuilt only when it changes.
#[derive(Clone, Debug, PartialEq)]
struct ListKey {
    rows: Vec<(Connection, Option<String>, bool)>,
    selected: Option<ConnId>,
    tor_global: bool,
    rev: u64,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "demo capture, seeding, loading and pending rebuilds are orthogonal state"
)]
pub struct VpnApp {
    shell: NavShell,
    intents: Intents,
    vault: Vault,
    /// Per-window selection is presentation state, not shared configuration.
    local_selected: Option<ConnId>,
    seeded: bool,
    detail_tab: DetailTab,
    modal: Modal,
    /// The open sheet, built when [`Self::modal`] (or what it shows) changes.
    dialog: Option<ModalDialog<Column>>,
    dialog_key: Option<String>,
    /// The sheet opened or closed since the host last laid the tree out.
    dialog_changed: bool,
    add_kind: ConnKind,
    pending: Option<Pending>,
    /// A host-confirmed resource operation waiting for the next mutable-doc
    /// sync pass. The widget operation hook intentionally receives a read-only
    /// document; tunnel status is committed here after the real command runs.
    queued_resource_toggle: Option<(ConnId, tilo_actions::InvocationId)>,
    completed_resource_toggle: Option<tilo_actions::InvocationUpdate>,
    queued_delete: Option<(ConnId, String, tilo_actions::InvocationId)>,
    completed_delete: Option<tilo_actions::InvocationUpdate>,
    /// Most recent action results, newest last. Starts empty — populated
    /// only by real user actions (imports, connect attempts).
    activity: Vec<String>,
    /// In-flight XDG-portal file import, drained once per frame. `None` when
    /// idle.
    import_rx: Option<PortalChannel>,
    #[cfg(test)]
    controlled_import_portal: Option<Box<dyn Fn(PickRequest) -> PortalChannel>>,
    #[cfg(test)]
    history_control_arm: Option<Rc<Cell<Option<ConnKind>>>>,
    /// Which protocol the pending import was requested for (seeds the parser
    /// when the chosen file has an ambiguous extension).
    import_hint: ConnKind,
    /// Existing shared profile being provisioned locally. `None` means a new
    /// import creates secret-free collaborative metadata after NM succeeds.
    provision_target: Option<ConnId>,
    import_invocation: Option<tilo_actions::InvocationId>,
    completed_import: Option<tilo_actions::InvocationUpdate>,
    pending_control_invocation: RawControlSlot,
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
    /// Parsed display facts for profiles provisioned on this device. This map
    /// is intentionally process-local and omitted from all durable/action
    /// types; `NetworkManager` remains the credential/config owner.
    local_details: HashMap<ConnId, LocalProfileDetails>,
    /// Last time the real interface state was reconciled, to throttle the
    /// per-frame `ip link` / `pgrep` probes to ~1 Hz.
    last_reconcile: Option<Instant>,
    /// Framework-owned action dispatch. Every document-mutating affordance (add
    /// a connection, toggle Tor) dispatches its [`crate::actions::VpnAction`]
    /// through this host, which brackets the undo boundary, encodes it, and
    /// applies it via the executor so it replays byte-identically on peers — the
    /// same path the Ctrl+K palette takes.
    actions: crate::resource::ActionSink,
    /// One-shot context supplied by the shell's typed tool service. It is
    /// consumed only after the persisted vault has loaded, so a profile route
    /// can select the real NetworkManager-backed connection by name.
    pending_tool_intent: Option<os_actions::ToolIntent>,
    demo_mode: bool,
    shot_loading: bool,
    /// A capture of the empty window: remove the saved sample on next sync.
    clear_saved: bool,
    /// Scroll offsets of the two columns, kept across rebuilds.
    list_offset: Rc<Cell<(f32, f32)>>,
    detail_offset: Rc<Cell<(f32, f32)>>,
    list_key: Option<ListKey>,
    detail_key: Option<String>,
    /// Bumped to force a rebuild that no model change implies (a switch the
    /// person flipped whose change was then cancelled).
    view_rev: u64,
    dirty: bool,
}

impl Default for VpnApp {
    fn default() -> Self {
        Self::new()
    }
}

impl VpnApp {
    /// The live app starts empty. Only the user's persisted tunnels (held in
    /// the shared document) appear.
    pub fn new() -> Self {
        let mut app = Self::build(Vault::default(), false);
        app.pending_tool_intent = os_actions::tool_intent_from_env();
        app
    }

    /// The hermetic `--shots` / demo build: seeded with a sample fleet so the
    /// screenshot is populated offline. This is the only place sample tunnels
    /// come from; the live app does not seed connections.
    pub fn new_demo() -> Self {
        Self::build(sample_vault(), true)
    }

    /// The live app, or the sample-seeded one for a capture
    /// (`TILO_VPN_SAMPLE=1`, set by the launcher for `--shots`).
    pub fn for_launch() -> Self {
        if std::env::var("TILO_VPN_SAMPLE").as_deref() == Ok("1") {
            Self::new_demo()
        } else {
            Self::new()
        }
    }

    fn build(vault: Vault, demo_mode: bool) -> Self {
        let local_selected = vault.selected;
        let live_status = vault
            .connections
            .iter()
            .map(|connection| (connection.id, connection.status))
            .collect();
        let local_credentials = if demo_mode {
            vault.connections.iter().map(|c| c.id).collect()
        } else {
            HashSet::new()
        };
        let local_details = if demo_mode {
            demo_local_details(&vault)
        } else {
            crate::local::load()
        };
        let intents = Intents::default();
        let shell = Self::new_shell(&intents, true);
        VpnApp {
            shell,
            intents,
            vault,
            local_selected,
            seeded: false,
            detail_tab: DetailTab::Status,
            modal: Modal::None,
            dialog: None,
            dialog_key: None,
            dialog_changed: false,
            add_kind: ConnKind::WireGuard,
            pending: None,
            queued_resource_toggle: None,
            completed_resource_toggle: None,
            queued_delete: None,
            completed_delete: None,
            activity: Vec::new(),
            import_rx: None,
            #[cfg(test)]
            controlled_import_portal: None,
            #[cfg(test)]
            history_control_arm: None,
            import_hint: ConnKind::WireGuard,
            provision_target: None,
            import_invocation: None,
            completed_import: None,
            pending_control_invocation: RawControlSlot::replace_one(),
            operation_error: None,
            retry_import_kind: None,
            throughput: HashMap::new(),
            live_status,
            local_credentials,
            local_details,
            last_reconcile: None,
            actions: crate::resource::ActionSink::default(),
            pending_tool_intent: None,
            demo_mode,
            shot_loading: false,
            clear_saved: false,
            list_offset: Rc::new(Cell::new((0.0, 0.0))),
            detail_offset: Rc::new(Cell::new((0.0, 0.0))),
            list_key: None,
            detail_key: None,
            view_rev: 0,
            dirty: true,
        }
    }

    /// The window: the connections' header with the import action, and the
    /// detail's header with History.
    fn new_shell(intents: &Intents, history: bool) -> NavShell {
        let content_actions: Vec<ui::widget::Element> = if history {
            vec![Box::new(ui::widget::history_button())]
        } else {
            Vec::new()
        };
        NavShell::new()
            .overview_header("Connections", vec![Box::new(view::import_button(intents))])
            .content_header("", content_actions)
            .with_id("vpn-shell")
    }

    /// Without the in-window History button, so the host's own History bar
    /// serves (tests of the host's control custody).
    #[cfg(test)]
    fn without_history_button(mut self) -> Self {
        self.shell = Self::new_shell(&self.intents, false);
        self.list_key = None;
        self.detail_key = None;
        self
    }

    /// Whether `id` can be connected from this device: its exact
    /// `NetworkManager` profile is known here.
    fn provisioned(&self, id: ConnId) -> bool {
        self.local_credentials.contains(&id)
            && self
                .local_details
                .get(&id)
                .is_some_and(|details| !details.network_manager_uuid.is_empty())
    }

    /// The measured rates of one tunnel (a fixed sample in the demo build).
    fn rates_for(&self, connection: &Connection) -> Rates {
        if !connection.is_connected() {
            return Rates::default();
        }
        if self.demo_mode {
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

    /// Configure a deterministic, hermetic visual-review state.
    pub fn stage_scenario(&mut self, scenario: Option<&str>) {
        let Some(name) = scenario else { return };
        // A phone or narrow capture stages the same state as its wide twin.
        let base = name
            .strip_prefix("phone-")
            .or_else(|| name.strip_suffix("-narrow"))
            .unwrap_or(name);
        match base {
            "confirm" => {
                if let Some(id) = self
                    .vault
                    .connections
                    .iter()
                    .find(|c| c.name == "Work")
                    .map(|c| c.id)
                {
                    self.select(id);
                    self.request_toggle(id);
                }
            }
            "add" | "import" => self.open_add(ConnKind::WireGuard),
            "detail" => self.shell.show(NavPane::Content),
            "config" => {
                self.detail_tab = DetailTab::Config;
                self.shell.show(NavPane::Content);
            }
            "logs" | "activity" => {
                self.detail_tab = DetailTab::Activity;
                self.shell.show(NavPane::Content);
                // Newest last: the tab lists the latest first.
                self.activity
                    .push("No keys were written to the shared document".into());
                self.activity
                    .push("NetworkManager confirmed the tunnel is active".into());
            }
            "loading" => self.shot_loading = true,
            "disconnected" => {
                if let Some(id) = self
                    .vault
                    .connections
                    .iter()
                    .find(|connection| connection.status == ConnStatus::Disconnected)
                    .map(|connection| connection.id)
                {
                    self.select(id);
                    self.live_status.insert(id, ConnStatus::Disconnected);
                }
            }
            "connecting" => {
                if let Some(id) = self
                    .vault
                    .connections
                    .iter()
                    .find(|connection| connection.status == ConnStatus::Disconnected)
                    .map(|connection| connection.id)
                {
                    self.select(id);
                    self.live_status.insert(id, ConnStatus::Connecting);
                    if let Some(connection) = self.vault.get_mut(id) {
                        connection.status = ConnStatus::Connecting;
                    }
                }
            }
            "error" => {
                self.operation_error =
                    Some("The profile is incomplete: the peer has no public key.".into());
                self.retry_import_kind = Some(ConnKind::WireGuard);
            }
            "empty" => {
                self.vault = Vault::default();
                self.local_selected = None;
                self.live_status.clear();
                self.local_credentials.clear();
                self.local_details.clear();
                // The sample was already saved; the next sync removes it
                // through the same action a person's delete takes.
                self.clear_saved = true;
            }
            "unprovisioned" | "setup" => {
                if let Some(id) = self
                    .vault
                    .connections
                    .iter()
                    .find(|c| c.name == "Work")
                    .map(|c| c.id)
                {
                    self.select(id);
                    self.local_credentials.remove(&id);
                    self.local_details.remove(&id);
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    // Keep the in-memory vault status aligned with live_status so
                    // shots/tests that read `app.vault` agree with published
                    // availability (doc-backed publish always stores Disconnected).
                    if let Some(connection) = self.vault.get_mut(id) {
                        connection.status = ConnStatus::Disconnected;
                    }
                    if base == "setup" {
                        self.provision_target = Some(id);
                        self.open_add(ConnKind::OpenVpn);
                    }
                }
            }
            _ => {}
        }
        self.dirty = true;
        self.refresh_view();
        self.refresh_sheet();
        self.shell.snap();
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

    /// Pull the vault from the CRDT. On first run, publish the connections we
    /// were built with — the user's saved tunnels, or (demo build) the sample.
    /// An empty live vault publishes nothing.
    fn refresh(&mut self, doc: &mut AutoCommit) {
        if !self.seeded {
            if VpnDoc::is_empty(doc) && !self.vault.connections.is_empty() {
                self.publish_vault(doc);
            }
            self.seeded = true;
        }
        self.vault = VpnDoc::read_vault(doc);
        for connection in &mut self.vault.connections {
            connection.status = self
                .live_status
                .get(&connection.id)
                .copied()
                .unwrap_or(ConnStatus::Disconnected);
            if self.demo_mode && connection.status == ConnStatus::Connected {
                connection.last_used = "Active now".into();
                connection.last_handshake_secs = Some(34);
            }
        }
        self.vault.connections.sort_by(|a, b| {
            b.is_connected()
                .cmp(&a.is_connected())
                .then_with(|| a.name.cmp(&b.name))
        });
        self.reconcile_real_status();
        self.local_selected = self
            .local_selected
            .filter(|id| self.vault.get(*id).is_some())
            .or_else(|| self.vault.connections.first().map(|c| c.id));
        self.vault.selected = self.local_selected;
    }

    /// Reconcile each tunnel's stored status against the kernel/process
    /// state (`ip link` for `WireGuard`, `pgrep` for `OpenVPN`). The stored toggle
    /// never overrides reality: a tunnel that dropped out-of-band flips back to
    /// Disconnected, and one brought up elsewhere shows Connected. Disabled
    /// in hermetic tests and throttled
    /// to ~1 Hz. `Connecting` is left alone — that transient is owned by the
    /// in-flight command.
    fn reconcile_real_status(&mut self) {
        if !config::exec_allowed() || self.demo_mode {
            return;
        }
        let now = ui::time::now();
        if let Some(prev) = self.last_reconcile
            && now.duration_since(prev).as_secs_f64() < 1.0
        {
            return;
        }
        self.last_reconcile = Some(now);

        let Ok(profiles) = os_actions::list_vpn_connections() else {
            return;
        };
        for connection in &mut self.vault.connections {
            let local = self.local_details.get(&connection.id).and_then(|details| {
                profiles
                    .iter()
                    .find(|profile| profile.uuid == details.network_manager_uuid)
            });
            let status = if local.is_some_and(|profile| profile.active) {
                ConnStatus::Connected
            } else {
                ConnStatus::Disconnected
            };
            if local.is_some() {
                self.local_credentials.insert(connection.id);
            } else {
                // Keep the local association as a recovery hint, but never
                // advertise machine actions after its exact NM UUID vanished.
                self.local_credentials.remove(&connection.id);
            }
            connection.status = status;
            self.live_status.insert(connection.id, status);
        }
    }

    /// Poll byte counters for every connected tunnel and fold them into the
    /// ephemeral throughput history. A missing interface counter is skipped.
    /// Disabled in hermetic tests and only runs for tunnels reporting Connected.
    fn sample_throughput(&mut self) {
        if !config::exec_allowed() || self.demo_mode {
            return;
        }
        let connected: Vec<ConnId> = self
            .vault
            .connections
            .iter()
            .filter(|c| c.is_connected())
            .map(|c| c.id)
            .collect();
        // Forget history for tunnels that are no longer up.
        self.throughput.retain(|id, _| connected.contains(id));
        for id in connected {
            if let Some(c) = self.vault.get(id)
                && let Some(xfer) = net::read_transfer(c)
            {
                self.throughput.entry(id).or_default().push(xfer);
            }
        }
    }

    // ---- mutations (all go through the CRDT) -------------------------

    fn select(&mut self, id: ConnId) {
        self.local_selected = Some(id);
        self.vault.selected = Some(id);
        self.detail_offset.set((0.0, 0.0));
        self.shell.show(NavPane::Content);
        self.dirty = true;
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

    fn request_toggle(&mut self, id: ConnId) {
        let Some(c) = self.vault.get(id) else { return };
        if !self.demo_mode && !self.local_credentials.contains(&id) {
            self.operation_error = Some(format!(
                "Credentials are unavailable on this device for '{}'. Import the exact provider profile here before connecting.",
                c.name
            ));
            self.retry_import_kind = Some(c.kind);
            return;
        }
        if c.status == ConnStatus::Connecting {
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
        let next = !self.vault.tor_global;
        self.pending = Some(Pending::TorGlobal(next, config::tor_plan(next)));
        self.modal = Modal::Confirm;
    }

    /// Apply the pending action (after the user confirms). State is changed
    /// **only if the command succeeded** — the UI never claims a tunnel is
    /// connected on the strength of an optimistic guess. `NetworkManager`
    /// remains the source of truth shared with quick settings.
    fn confirm_pending(&mut self, doc: &mut AutoCommit) {
        let Some(p) = self.pending.take() else { return };
        match p {
            Pending::Connect(id, plan) => match config::execute(&plan) {
                Ok(out) => {
                    let profile_uuid = self
                        .local_details
                        .get(&id)
                        .map(|details| details.network_manager_uuid.as_str());
                    let real_up = profile_uuid.map(config::network_manager_active).transpose();
                    if matches!(real_up, Ok(Some(true))) {
                        self.live_status.insert(id, ConnStatus::Connected);
                        self.log(format!("$ {}  → up {}", plan.display(), out));
                    } else {
                        self.live_status.insert(id, ConnStatus::Disconnected);
                        let reason = match real_up {
                            Ok(Some(false)) => {
                                "NetworkManager still reports the profile inactive".into()
                            }
                            Ok(Some(true)) => "the readiness state changed unexpectedly".into(),
                            Ok(None) => "the saved profile no longer exists".into(),
                            Err(error) => error,
                        };
                        self.operation_error = Some(format!(
                            "NetworkManager accepted the request, but readiness could not be confirmed: {reason}"
                        ));
                        self.log(format!("$ {}  → not ready: {reason}", plan.display()));
                    }
                }
                Err(e) => {
                    // Stay disconnected — we did NOT connect.
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    self.operation_error = Some(format!("Could not connect: {e}"));
                    self.log(format!("$ {}  → not connected: {}", plan.display(), e));
                }
            },
            Pending::Disconnect(id, plan) => match config::execute(&plan) {
                Ok(out) => {
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    self.log(format!("$ {}  → ok {}", plan.display(), out));
                }
                Err(e) => {
                    self.operation_error = Some(format!("Could not disconnect: {e}"));
                    self.log(format!("$ {}  → failed: {}", plan.display(), e));
                }
            },
            Pending::TorGlobal(on, plan) => match config::execute(&plan) {
                Ok(out) => {
                    // The routing command is the external side effect; flipping
                    // the stored preference is the action (undo + replay for free).
                    let action = actions::set_tor_global_action(on);
                    self.actions.dispatch_with_context(
                        doc,
                        VPN_ACTION_TYPE,
                        &action,
                        tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
                    );
                    self.log(format!("$ {}  → ok {}", plan.display(), out));
                }
                Err(e) => self.log(format!("$ {}  → failed: {}", plan.display(), e)),
            },
        }
        self.modal = Modal::None;
        self.refresh(doc);
    }

    fn log(&mut self, line: String) {
        self.activity.push(line);
        if self.activity.len() > 50 {
            let drop = self.activity.len() - 50;
            self.activity.drain(0..drop);
        }
    }

    // ---- import (XDG portal) -----------------------------------------

    /// Ask the host desktop portal to pick a `.conf` / `.ovpn`. Non-blocking:
    /// the D-Bus round-trip + host dialog run on a worker thread; the chosen
    /// path arrives over a channel drained by [`Self::drain_import`]. An
    /// in-flight request is not re-opened.
    fn request_import(&mut self, kind: ConnKind) {
        if self.import_rx.is_some() {
            return;
        }
        self.operation_error = None;
        self.retry_import_kind = None;
        self.import_hint = kind;
        let request = tunnel_import_request(kind);
        #[cfg(test)]
        let receiver = if let Some(portal) = &self.controlled_import_portal {
            portal(request)
        } else {
            request.send()
        };
        #[cfg(not(test))]
        let receiver = request.send();
        self.import_rx = Some(receiver);
        self.log(format!("Choose a .{} file to import", kind.ext()));
    }

    fn request_local_provision(&mut self, id: ConnId) {
        let Some(connection) = self.vault.get(id) else {
            return;
        };
        self.provision_target = Some(id);
        self.request_import(connection.kind);
    }

    fn queue_import(&mut self, kind: ConnKind) {
        self.provision_target = None;
        let action = match kind {
            ConnKind::WireGuard => "import-wireguard",
            ConnKind::OpenVpn => "import-openvpn",
        };
        let _ = self.pending_control_invocation.emit(ControlInvocation {
            address: ActionAddress::new("vpn.vault", action),
            arguments: serde_json::Map::new(),
        });
    }

    /// Drain an in-flight import. When the user picked a file we read it,
    /// parse the WireGuard/OpenVPN config into a [`Connection`], and add
    /// it to the vault. Returns true if anything changed.
    #[allow(
        clippy::too_many_lines,
        reason = "one linear transaction keeps bounds, validation, zeroization, OS import, and collaborative commit visibly ordered"
    )]
    fn drain_import(&mut self, doc: &mut AutoCommit) -> bool {
        let Some(rx) = &self.import_rx else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(o) => o,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.import_rx = None;
                return false;
            }
        };
        self.import_rx = None;
        match outcome {
            PortalOutcome::Opened(path) => {
                let file_len = std::fs::metadata(&path).map(|meta| meta.len());
                if file_len.is_ok_and(|len| len > config::MAX_CONFIG_BYTES) {
                    self.finish_import_error(format!(
                        "The selected profile exceeds the {} KiB safety limit.",
                        config::MAX_CONFIG_BYTES / 1024
                    ));
                    return true;
                }
                match std::fs::read_to_string(&path) {
                    Ok(mut text) => {
                        let stem = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Imported")
                            .to_string();
                        let kind = config::kind_from_filename(
                            path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
                        )
                        .unwrap_or(self.import_hint);
                        if let Err(error) = config::validate_config(kind, &text) {
                            text.zeroize();
                            self.finish_import_error(error);
                            return true;
                        }
                        let target = self.provision_target;
                        let conn = if let Some(id) = target {
                            let Some(existing) = self.vault.get(id) else {
                                text.zeroize();
                                self.finish_import_error("This shared profile was removed before local provisioning completed.".into());
                                return true;
                            };
                            if existing.kind != kind {
                                text.zeroize();
                                self.finish_import_error("The selected provider file has a different VPN protocol than this shared profile.".into());
                                return true;
                            }
                            existing.clone()
                        } else {
                            config::parse_config(&stem, kind, &text)
                        };
                        let mut local_details = config::parse_local_details(kind, &text);
                        // Hand the portal-selected file straight to NetworkManager.
                        // It becomes the sole owner of the profile and its secrets;
                        // Studio deliberately keeps no second raw private-key copy.
                        let nm_kind = match kind {
                            ConnKind::WireGuard => "wireguard",
                            ConnKind::OpenVpn => "openvpn",
                        };
                        let known_uuids = if config::exec_allowed() {
                            match os_actions::list_vpn_connections() {
                                Ok(profiles) => {
                                    profiles.into_iter().map(|profile| profile.uuid).collect()
                                }
                                Err(error) => {
                                    text.zeroize();
                                    self.finish_import_error(format!("NetworkManager availability could not be verified before import: {error}"));
                                    return true;
                                }
                            }
                        } else {
                            HashSet::new()
                        };
                        let import_result = config::exec_allowed().then(|| {
                            os_actions::import_vpn_config(nm_kind, &path.to_string_lossy())
                        });
                        text.zeroize();
                        match import_result {
                            None => self.log(format!(
                                "Read the {} profile '{}'; NetworkManager import is off in tests",
                                kind.long_name(),
                                stem
                            )),
                            Some(Ok(())) => {
                                let discovered = os_actions::list_vpn_connections()
                                    .map(|profiles| {
                                        profiles
                                            .into_iter()
                                            .filter(|profile| !known_uuids.contains(&profile.uuid))
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                let Some(discovered) = (discovered.len() == 1)
                                    .then(|| discovered.into_iter().next())
                                    .flatten()
                                else {
                                    self.finish_import_recovery_error("NetworkManager imported a profile, but its exact new UUID could not be identified. The external profile exists; recover or remove it in NetworkManager before attempting another import.".into());
                                    return true;
                                };
                                if discovered.uuid.is_empty() {
                                    self.finish_import_recovery_error("NetworkManager imported a profile, but its exact new UUID could not be identified. The external profile exists; recover or remove it in NetworkManager before attempting another import.".into());
                                    return true;
                                }
                                local_details.network_manager_uuid = discovered.uuid;
                                self.log(format!(
                                    "Imported the {} profile '{}' into NetworkManager",
                                    kind.long_name(),
                                    stem
                                ));
                            }
                            Some(Err(e)) => {
                                self.finish_import_error(format!(
                                    "NetworkManager could not import {} '{}': {e}",
                                    kind.badge(),
                                    stem
                                ));
                                return true;
                            }
                        }
                        let id = conn.id;
                        // NetworkManager import above is the external side effect;
                        // adding the resulting profile to the shared document is the
                        // same action the add form dispatches — one source of truth.
                        if target.is_none() {
                            let action = actions::add_connection_action(conn);
                            self.actions.dispatch_with_context(
                                doc,
                                VPN_ACTION_TYPE,
                                &action,
                                tilo_actions::ActionContext::for_action(
                                    "so.tilo.vpn",
                                    "VPN",
                                    &action,
                                ),
                            );
                        } else {
                            // Local setup associates credentials only — no
                            // collaborative AddConnection. Clear any seed
                            // publish so post-setup inspection does not treat
                            // demo vault actions as this operation.
                            self.actions.clear_last_dispatched();
                        }
                        self.refresh(doc);
                        let mut persistence_error = None;
                        if !local_details.network_manager_uuid.is_empty() {
                            self.local_credentials.insert(id);
                            self.local_details.insert(id, local_details);
                            if let Err(error) = crate::local::save(&self.local_details) {
                                persistence_error = Some(format!(
                                    "Profile imported but its device-local association could not be saved: {error}. The external profile exists; do not retry import, reopen NetworkManager to recover it."
                                ));
                            }
                        }
                        self.select(id);
                        if let Some(error) = persistence_error {
                            self.operation_error = Some(error.clone());
                            self.retry_import_kind = None;
                            if let Some(invocation) = self.import_invocation.take() {
                                self.completed_import =
                                    Some(tilo_actions::InvocationUpdate::Failed {
                                        invocation,
                                        error: tilo_actions::ActionFailure {
                                            code: "vpn.local_state_failed".into(),
                                            message: error,
                                            details: None,
                                            retryable: false,
                                        },
                                    });
                            }
                        } else {
                            self.operation_error = None;
                            self.finish_import_success();
                        }
                        self.provision_target = None;
                        true
                    }
                    Err(e) => {
                        self.finish_import_error(format!(
                            "Could not read the selected profile: {e}"
                        ));
                        true
                    }
                }
            }
            PortalOutcome::Cancelled => {
                self.provision_target = None;
                if let Some(invocation) = self.import_invocation.take() {
                    self.completed_import = Some(tilo_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("Import cancelled".into()),
                    });
                }
                false
            }
            PortalOutcome::Failed(reason) => {
                self.finish_import_error(format!("The file picker failed: {reason}"));
                true
            }
        }
    }

    fn finish_import_success(&mut self) {
        self.operation_error = None;
        self.retry_import_kind = None;
        if let Some(invocation) = self.import_invocation.take() {
            self.completed_import = Some(tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            });
        }
    }

    fn finish_import_error(&mut self, message: String) {
        self.operation_error = Some(message.clone());
        self.retry_import_kind = Some(self.import_hint);
        self.log(message.clone());
        if let Some(invocation) = self.import_invocation.take() {
            self.completed_import = Some(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "vpn.import_failed".into(),
                    message,
                    details: None,
                    retryable: true,
                },
            });
        }
    }

    /// A post-import ambiguity is not retryable: retrying could create a
    /// duplicate credential-bearing `NetworkManager` profile.
    fn finish_import_recovery_error(&mut self, message: String) {
        self.operation_error = Some(message.clone());
        self.retry_import_kind = None;
        self.log(message.clone());
        if let Some(invocation) = self.import_invocation.take() {
            self.completed_import = Some(tilo_actions::InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "vpn.import_identity_ambiguous".into(),
                    message,
                    details: None,
                    retryable: false,
                },
            });
        }
    }

    pub(crate) fn toggle_conn_tor(&mut self, doc: &mut AutoCommit, id: ConnId) {
        let next = self.vault.get(id).is_none_or(|c| !c.tor);
        let action = actions::set_conn_tor_action(id, next);
        self.actions.dispatch_with_context(
            doc,
            VPN_ACTION_TYPE,
            &action,
            tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
        );
        self.refresh(doc);
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

    /// Reopen the exact import protocol that failed. This only runs when the
    /// failure was produced by the portal/import pipeline, never for a tunnel
    /// transition where replaying a privileged command blindly would be unsafe.
    fn retry_import(&mut self) {
        let Some(kind) = self.retry_import_kind else {
            return;
        };
        self.request_import(kind);
    }

    pub(crate) fn commit_add(&mut self) {
        if self.provision_target.is_some() {
            self.request_import(self.add_kind);
        } else {
            self.queue_import(self.add_kind);
        }
        self.modal = Modal::None;
        self.log(format!(
            "Waiting for approval to import a {} configuration",
            self.add_kind.long_name()
        ));
    }

    /// The last [`tilo_actions::EncodedAction`] an affordance dispatched, for the
    /// `affordance == ActionSpec` covenant test — an on-screen control and its
    /// Ctrl+K entry must build the byte-identical action.
    #[cfg(test)]
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&tilo_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// Show the saved tunnels the framework resource already holds.
    /// An empty projection does not wipe tunnels that have not been published yet.
    pub(crate) fn adopt_resource_state(&mut self, state: &crate::model::Vault) {
        if state.connections.is_empty() && !self.vault.connections.is_empty() {
            return;
        }
        self.vault.connections.clone_from(&state.connections);
        self.vault.tor_global = state.tor_global;
        if self
            .vault
            .selected
            .is_some_and(|id| self.vault.get(id).is_none())
        {
            self.vault.selected = None;
        }
    }

    /// Attach the framework dispatcher when hosted as a [`ResourceSurface`].
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::Vpn>,
    ) {
        self.actions.attach(dispatcher);
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.actions.mark_hosted_for_test();
    }

    fn publish_vault(&mut self, doc: &mut AutoCommit) {
        let tor_global = self.vault.tor_global;
        for connection in self.vault.connections.clone() {
            let action = actions::add_connection_action(connection);
            self.actions.dispatch_with_context(
                doc,
                VPN_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
            );
        }
        if tor_global {
            let action = actions::set_tor_global_action(true);
            self.actions.dispatch_with_context(
                doc,
                VPN_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
            );
        }
    }
}

/// Build a device-local activity view. It is not persisted, shared or indexed.
fn synth_logs(
    c: &Connection,
    details: Option<&LocalProfileDetails>,
    activity: &[String],
    xfer: Option<Transfer>,
) -> Vec<String> {
    let mut out = Vec::new();
    if c.is_connected() {
        out.push(format!(
            "{} tunnel is up on this device",
            c.kind.long_name()
        ));
        if let Some(details) = details {
            out.push(format!("Endpoint {}", details.endpoint));
        }
        if let Some(x) = xfer {
            out.push(format!(
                "Received {}, sent {}",
                ui::format::bytes(x.rx),
                ui::format::bytes(x.tx)
            ));
        }
        if c.tor {
            out.push("Traffic is routed over Tor".into());
        }
    } else {
        out.push("The tunnel is down on this device".into());
    }
    // append global activity newest-last
    for a in activity.iter().rev().take(8) {
        out.push(a.clone());
    }
    out
}

// ---- the shell's columns and the sheets -------------------------------------

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
        (self.import_rx.is_some() || self.shot_loading)
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
                        self.provisioned(c.id),
                    )
                })
                .collect(),
            selected: self.vault.selected,
            tor_global: self.vault.tor_global,
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
                        provisioned: self.provisioned(connection.id),
                    })
                    .collect(),
                selected: self.vault.selected,
                tor_global: self.vault.tor_global,
            };
            let overview = view::overview(&model, &self.intents, &self.list_offset);
            self.shell.set_overview(Some(overview));
            self.list_key = Some(list_key);
            self.dirty = true;
        }

        let Some(connection) = self.vault.selected_conn() else {
            return;
        };
        let details = self.local_details.get(&connection.id);
        let provisioned = self.provisioned(connection.id);
        let rates = self.rates_for(connection);
        let activity = synth_logs(connection, details, &self.activity, rates.latest);
        let key = format!(
            "{connection:?}{details:?}{provisioned}{:?}{rates:?}{activity:?}{notice_state:?}{}",
            self.detail_tab, self.view_rev
        );
        if self.detail_key.as_deref() == Some(key.as_str()) {
            return;
        }
        let model = view::DetailModel {
            connection,
            details,
            provisioned,
            tab: self.detail_tab,
            rates: &rates,
            activity,
            notice,
        };
        let content = view::detail(&model, &self.intents, &self.detail_offset);
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
        let was_open = self.dialog.is_some();
        self.dialog = key.as_ref().and_then(|_| self.build_dialog());
        self.dialog_key = key;
        if was_open != self.dialog.is_some() {
            self.dialog_changed = true;
        }
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
                Some(
                    ModalDialog::new(title, view::confirm_body(&summary, &command))
                        .open(true)
                        .with_test_id("vpn-confirm-sheet")
                        .primary_action(primary, intents.sender(Intent::Confirm))
                        .secondary_action("Cancel", intents.sender(Intent::CancelConfirm))
                        .on_dismiss(intents.sender(Intent::CancelConfirm)),
                )
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
        match MainAction::of(connection, self.provisioned(id)) {
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
    fn drain_intents(&mut self, doc: &mut AutoCommit) {
        for intent in self.intents.take() {
            match intent {
                Intent::Select(id) => self.select(id),
                Intent::SetTab(tab) => {
                    self.detail_tab = tab;
                    self.detail_offset.set((0.0, 0.0));
                }
                Intent::MainAction(id) => self.main_action(id),
                Intent::ToggleTor(id) => self.toggle_conn_tor(doc, id),
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
                    self.confirm_pending(doc);
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

    /// Remove every saved connection through the same action a person's
    /// delete takes (the capture of the empty window).
    fn remove_saved(&mut self, state: &mut AutoCommit) {
        for connection in VpnDoc::read_vault(state).connections {
            let action = actions::remove_connection_action(connection.id);
            self.actions.dispatch_with_context(
                state,
                VPN_ACTION_TYPE,
                &action,
                tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
            );
        }
    }

    /// Fold the shared document into the widget's own state and settle
    /// host-confirmed operations.
    fn reconcile(&mut self, state: &mut AutoCommit) {
        #[cfg(test)]
        if let Some(kind) = self.history_control_arm.as_ref().and_then(|arm| arm.take()) {
            self.queue_import(kind);
        }
        if std::mem::take(&mut self.clear_saved) {
            self.remove_saved(state);
        }
        // Fold in a completed portal import (if any), then read the vault and
        // reconcile real interface state, then poll real throughput counters.
        self.drain_import(state);
        self.refresh(state);
        if let Some((id, invocation)) = self.queued_resource_toggle.take() {
            let was_connected = self.vault.get(id).is_some_and(Connection::is_connected);
            self.request_toggle(id);
            self.confirm_pending(state);
            let is_connected = self.vault.get(id).is_some_and(Connection::is_connected);
            self.completed_resource_toggle = Some(if was_connected == is_connected {
                tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "vpn.tunnel_transition_failed".into(),
                        message: self
                            .activity
                            .last()
                            .cloned()
                            .unwrap_or_else(|| "VPN tunnel state did not change".into()),
                        details: None,
                        retryable: false,
                    },
                }
            } else {
                tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                }
            });
        }
        if let Some((id, uuid, invocation)) = self.queued_delete.take() {
            self.completed_delete = Some(match os_actions::delete_vpn_connection_uuid(&uuid) {
                Ok(()) => {
                    let removed_details = self.local_details.remove(&id);
                    self.local_credentials.remove(&id);
                    if let Err(error) = crate::local::save(&self.local_details) {
                        if let Some(details) = removed_details {
                            self.local_details.insert(id, details);
                        }
                        self.operation_error = Some(format!(
                            "NetworkManager deleted the profile, but local association cleanup failed: {error}. The shared profile was retained for recovery."
                        ));
                        tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "vpn.local_state_failed".into(),
                                message:
                                    "NetworkManager deleted the profile but local cleanup failed"
                                        .into(),
                                details: None,
                                retryable: false,
                            },
                        }
                    } else {
                        let action = actions::remove_connection_action(id);
                        self.actions.dispatch_with_context(
                            state,
                            VPN_ACTION_TYPE,
                            &action,
                            tilo_actions::ActionContext::for_action("so.tilo.vpn", "VPN", &action),
                        );
                        self.local_selected = None;
                        self.live_status.remove(&id);
                        self.operation_error = None;
                        tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        }
                    }
                }
                Err(message) => {
                    self.operation_error =
                        Some(format!("Could not delete the local VPN profile: {message}"));
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "vpn.delete_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    }
                }
            });
            self.refresh(state);
        }
        self.apply_pending_tool_intent();
        self.sample_throughput();
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

    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
            return;
        }
        self.shell.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::undo::CommittedActions>() {
            request.provide::<dyn ui::undo::CommittedActions>(self);
            return;
        }
        if request.wants::<dyn ui::widget::controls::ActionControls>() {
            request.provide::<dyn ui::widget::controls::ActionControls>(self);
            return;
        }
        self.shell.provide_mut(request);
    }

    fn test_id(&self) -> Option<&str> {
        Some("vpn_app")
    }

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            actions::VPN_TOGGLE_TYPE,
            actions::VPN_IMPORT_TYPE,
            actions::VPN_DELETE_TYPE,
            actions::VPN_PROVISION_TYPE,
        ]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(&mut self.shell, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(&mut self.shell, op, doc, path)? {
            return Ok(Some(started));
        }
        self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        self.poll_operation(invocation, cancellation_requested)
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.reconcile(doc);
            self.drain_intents(doc);
        }
        self.refresh_view();
        self.refresh_sheet();
        self.shell.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        {
            let (doc, _) = ui::doc::document(state, path);
            self.drain_intents(doc);
        }
        self.refresh_view();
        self.refresh_sheet();
        self.shell.update(state, path) || self.dirty
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The open sheet is modal: it takes every event, and nothing reaches
        // the window behind its scrim.
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.handle_detached(event, ctx.scale_factor, *ctx.modifiers, ctx.state, ctx.path);
            *ctx.handled = true;
            // The sheet's hover and press faces repaint with the window.
            self.dirty = true;
            self.drain_intents(ctx.state);
            return;
        }
        self.shell.handle_event(event, ctx);
        self.drain_intents(ctx.state);
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
        self.import_rx.is_some()
            || (config::exec_allowed()
                && !self.demo_mode
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

impl VpnApp {
    fn poll_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if self
            .completed_import
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_import.take();
        }
        if self
            .completed_delete
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_delete.take();
        }
        if cancellation_requested && self.import_invocation == Some(invocation) {
            self.import_invocation = None;
            self.import_rx = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("Import cancelled before a profile was selected".into()),
            });
        }
        if cancellation_requested
            && self
                .queued_resource_toggle
                .is_some_and(|(_, pending)| pending == invocation)
        {
            self.queued_resource_toggle = None;
            return Some(tilo_actions::InvocationUpdate::Cancelled {
                invocation,
                message: Some("cancelled before the VPN command was run".into()),
            });
        }
        if self
            .completed_resource_toggle
            .as_ref()
            .is_some_and(|update| update.invocation() == invocation)
        {
            return self.completed_resource_toggle.take();
        }
        None
    }

    /// A key no control took: Escape dismisses a problem, the arrows move
    /// through the connections, and 1–3 pick a tab.
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

impl ui::resources::PublishesResources for VpnApp {
    /// Standalone / `--shots` / unit tests publish the full vault tree and
    /// overlay live `NetworkManager` availability on connect / disconnect /
    /// delete. When hosted, [`tilo_resource::publish`] already owns that door
    /// (including tunnel Effects on connection children) — return nothing so
    /// we do not republish a second `vpn.vault` root.
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.actions.is_hosted() {
            return Vec::new();
        }
        self.connection_resources(doc)
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        fn find(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource.clone());
                }
                if let Some(found) = find(&resource.children, id) {
                    return Some(found);
                }
            }
            None
        }
        if self.actions.is_hosted() && id == "vpn.vault" {
            return None;
        }
        find(&self.connection_resources(doc), id)
    }
}

impl VpnApp {
    fn connection_resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let from_doc = VpnDoc::read_vault(doc);
        let vault = if from_doc.connections.is_empty() && !self.vault.connections.is_empty() {
            &self.vault
        } else {
            &from_doc
        };
        vec![self.published_vault(vault)]
    }
}

impl VpnApp {
    /// The vault door from [`tilo_resource::publish`], with this device's
    /// tunnel availability written onto connect, disconnect, and delete.
    pub(crate) fn published_vault(&self, vault: &crate::model::Vault) -> tilo_actions::Resource {
        let mut root =
            ui::tilo_resource::publish::<crate::resource::Vpn>("vpn.vault", "VPN", vault);
        for connection in &vault.connections {
            let Some(resource) = root
                .children
                .iter_mut()
                .find(|resource| resource.id.0 == format!("vpn.connection:{}", connection.id))
            else {
                continue;
            };
            let connecting = connection.status == ConnStatus::Connecting;
            let locally_provisioned = self.local_credentials.contains(&connection.id)
                && self
                    .local_details
                    .get(&connection.id)
                    .is_some_and(|details| !details.network_manager_uuid.is_empty());
            let tunnel_availability = if connecting {
                tilo_actions::Availability::Unavailable {
                    code: "transition_in_progress".into(),
                    reason: "The tunnel is already changing state".into(),
                }
            } else if !locally_provisioned {
                tilo_actions::Availability::Unavailable {
                    code: "local_credentials_required".into(),
                    reason: "Set up this shared profile on this device before connecting".into(),
                }
            } else {
                tilo_actions::Availability::Available
            };
            for action in &mut resource.actions {
                match action.id.as_str() {
                    "delete" => {
                        action.contract.availability = if locally_provisioned {
                            tilo_actions::Availability::Available
                        } else {
                            tilo_actions::Availability::Unavailable {
                                    code: "local_credentials_required".into(),
                                    reason: "No exact local NetworkManager profile is provisioned on this device".into(),
                                }
                        };
                    }
                    "connect" => {
                        action.contract.availability = if connection.is_connected() {
                            tilo_actions::Availability::Unavailable {
                                code: "already_connected".into(),
                                reason: "This tunnel is already up on this device".into(),
                            }
                        } else {
                            tunnel_availability.clone()
                        };
                    }
                    "disconnect" => {
                        action.contract.availability = if connection.is_connected() {
                            tunnel_availability.clone()
                        } else {
                            tilo_actions::Availability::Unavailable {
                                code: "not_connected".into(),
                                reason: "This tunnel is not up on this device".into(),
                            }
                        };
                    }
                    _ => {}
                }
            }
        }
        root
    }
}

impl ui::widget::controls::ActionControls for VpnApp {
    fn install_control_custody(
        &mut self,
        domain: &ui::ControlDomain,
        attachment: ui::control_custody::CustodyAttachment,
    ) {
        let _ = self
            .pending_control_invocation
            .install_custody(domain, attachment);
        ui::widget::controls::install_control_custody(&mut self.shell, domain, attachment);
    }
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_control_invocation
            .take_legacy_compatibility()
            .or_else(|| ui::widget::controls::take_control_invocation(&mut self.shell))
    }
}

impl ui::undo::CommittedActions for VpnApp {
    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing it — the
    /// same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl VpnApp {
    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != tilo_actions::OperationKind::Effect || type_id != actions::VPN_TOGGLE_TYPE {
            if kind == tilo_actions::OperationKind::Effect && type_id == actions::VPN_DELETE_TYPE {
                if self.queued_delete.is_some() {
                    return Err("another VPN profile deletion is already queued".into());
                }
                let (id, name): (ConnId, String) =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let connection = self
                    .vault
                    .get(id)
                    .ok_or_else(|| "VPN profile is no longer saved".to_string())?;
                if connection.name != name {
                    return Err("VPN profile changed before deletion was confirmed".into());
                }
                let uuid = self.local_details.get(&id)
                    .map(|details| details.network_manager_uuid.clone())
                    .filter(|uuid| !uuid.is_empty())
                    .ok_or_else(|| "Credentials are unavailable on this device; local NetworkManager profile cannot be deleted here".to_string())?;
                self.queued_delete = Some((id, uuid, invocation));
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            if kind == tilo_actions::OperationKind::Effect && type_id == actions::VPN_IMPORT_TYPE {
                if self.import_rx.is_some() || self.import_invocation.is_some() {
                    return Err("another VPN profile import is already in progress".into());
                }
                let raw: String = serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let kind = match raw.as_str() {
                    "wireguard" => ConnKind::WireGuard,
                    "openvpn" => ConnKind::OpenVpn,
                    _ => return Err("unsupported VPN profile type".into()),
                };
                self.import_invocation = Some(invocation);
                self.provision_target = None;
                self.request_import(kind);
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            if kind == tilo_actions::OperationKind::Effect && type_id == actions::VPN_PROVISION_TYPE
            {
                if self.import_rx.is_some() || self.import_invocation.is_some() {
                    return Err("another VPN profile import is already in progress".into());
                }
                let (raw, expected_kind): (String, String) =
                    serde_json::from_slice(payload).map_err(|e| e.to_string())?;
                let id = raw
                    .parse()
                    .map_err(|_| "Invalid VPN connection id".to_string())?;
                let (profile_id, actual_kind) = self
                    .vault
                    .get(id)
                    .map(|connection| (connection.id, connection.kind))
                    .ok_or_else(|| "VPN profile is no longer saved".to_string())?;
                if actual_kind.as_str() != expected_kind {
                    return Err("VPN profile protocol changed before setup started".into());
                }
                self.import_invocation = Some(invocation);
                self.request_local_provision(profile_id);
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            return Ok(None);
        }
        if self.queued_resource_toggle.is_some() {
            return Err("another VPN tunnel change is queued".into());
        }
        let raw: String = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
        let id = raw
            .parse()
            .map_err(|_| "Invalid VPN connection id".to_string())?;
        let connection = self
            .vault
            .get(id)
            .ok_or_else(|| "VPN connection is no longer saved".to_string())?;
        if connection.status == ConnStatus::Connecting {
            return Err("The tunnel is already changing state".into());
        }
        self.queued_resource_toggle = Some((id, invocation));
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use taffy::{NodeId, TaffyTree};
    use ui::headless::Framed;
    use vello::kurbo::Point;

    static CUSTODY_HISTORY: ui::history_mode::HistoryDefinition =
        ui::history_mode::HistoryDefinition {
            app_id: "dev.tilo.vpn.controlled-custody",
            scopes: &[ui::history_mode::HistoryScopeDefinition {
                key: "vpn-controlled-document",
                title: "VPN controlled document",
                presentation: ui::history_mode::HistoryPresentation::External {
                    reason: "controlled in-memory VPN custody fixture",
                },
                content_schema: None,
            }],
        };

    struct CustodyVpnRoot {
        child: Box<dyn Widget>,
        denied: Rc<Cell<bool>>,
    }

    impl Widget for CustodyVpnRoot {
        ui::provides!(
            dyn ui::history_mode::capability::DocumentHistory,
            dyn ui::widget::controls::ActionControls
        );

        fn name(&self) -> &'static str {
            "CustodyVpnRoot"
        }
        fn sync(&mut self, doc: &mut ui::widget::AppModel, path: &ui::widget::ModelPath) {
            self.child.sync(doc, path);
        }
        fn build_layout(
            &mut self,
            tree: &mut TaffyTree<()>,
            theme: &Theme,
            nodes: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.child.build_layout(tree, theme, nodes)
        }
        fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
            self.child.apply_layout(tree, node, origin);
        }
        fn draw(
            &mut self,
            scene: &mut Scene,
            transform: Affine,
            text: &mut TextRenderer,
            theme: &Theme,
            presence: &PresenceCtx,
        ) {
            self.child.draw(scene, transform, text, theme, presence);
        }
        fn get_rect(&self) -> Rect {
            self.child.get_rect()
        }
        fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
            self.child.handle_event(event, ctx);
        }
        fn children(&self) -> Vec<&dyn Widget> {
            vec![self.child.as_ref()]
        }
        fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
            if self.denied.get() {
                Vec::new()
            } else {
                vec![self.child.as_mut()]
            }
        }
    }

    impl ui::widget::controls::ActionControls for CustodyVpnRoot {
        fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
            if self.denied.get() {
                None
            } else {
                ui::widget::controls::take_control_invocation(self.child.as_mut())
            }
        }
    }

    impl ui::history_mode::capability::DocumentHistory for CustodyVpnRoot {
        fn document_history_definition(
            &self,
        ) -> Option<&'static ui::history_mode::HistoryDefinition> {
            Some(&CUSTODY_HISTORY)
        }
    }

    fn custody_driver(root: Box<dyn Widget>, doc: AutoCommit) -> ui::headless::Driver {
        let registration = ui::history_mode::HistoryRegistration::new(
            &CUSTODY_HISTORY,
            vec![ui::history_mode::HistorySourceRegistration::Unversioned {
                scope: "vpn-controlled-document",
                reason: "controlled in-memory VPN custody fixture",
            }],
        )
        .unwrap();
        let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/sdk/ui-core/assets/fonts"),
            "en-US",
        );
        let renderer = ui::TextRenderer::with_font_catalog(&catalog).unwrap();
        ui::headless::Driver::from_managed_root_with_controlled_inputs(
            ui::State::from_doc(doc),
            ui::history_mode::ManagedRoot::new(root, registration),
            (900, 700),
            ui::Theme::dark(),
            renderer,
            ui::permissions::UserId("vpn-custody-author".into()),
        )
        .unwrap()
    }

    /// Where a published control with `label` is, for a pointer to press it.
    fn published_bounds(driver: &ui::headless::Driver, label: &str) -> Rect {
        fn access(node: &ui::a11y::AccessNode, label: &str) -> Option<Rect> {
            if node.name.as_deref() == Some(label) {
                return node.bounds.map(ui::a11y::AccessBounds::to_rect);
            }
            node.children.iter().find_map(|node| access(node, label))
        }
        fn widget(node: &ui::headless::InspectionNode, label: &str) -> Option<Rect> {
            node.semantics
                .as_ref()
                .and_then(|node| access(node, label))
                .or_else(|| node.children.iter().find_map(|node| widget(node, label)))
        }
        widget(&driver.inspection().root, label)
            .unwrap_or_else(|| panic!("missing published control {label:?}"))
    }

    fn palette_control(driver: &ui::headless::Driver, label: &str) -> (String, Option<String>) {
        fn access(node: &ui::a11y::AccessNode, label: &str) -> Option<(String, Option<String>)> {
            if node.name.as_deref() == Some(label) {
                return node
                    .action_target
                    .clone()
                    .map(|target| (target, node.description.clone()));
            }
            node.children.iter().find_map(|node| access(node, label))
        }
        fn widget(
            node: &ui::headless::InspectionNode,
            label: &str,
        ) -> Option<(String, Option<String>)> {
            node.semantics
                .as_ref()
                .and_then(|node| access(node, label))
                .or_else(|| node.children.iter().find_map(|node| widget(node, label)))
        }
        widget(&driver.inspection().root, label)
            .unwrap_or_else(|| panic!("missing published palette control {label:?}"))
    }

    #[test]
    fn control_custody_vpn_unreachable_private_route_saves_the_armed_import_and_confirms_a_fresh_one()
     {
        let (app, doc) = demo_with_doc();
        let mut app = app.without_history_button();
        let arm = Rc::new(Cell::new(None));
        app.history_control_arm = Some(arm.clone());
        let portal_attempts = Rc::new(std::cell::RefCell::new(Vec::new()));
        let ledger = portal_attempts.clone();
        app.controlled_import_portal = Some(Box::new(move |request| {
            ledger.borrow_mut().push(
                request
                    .filter_names()
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
            );
            {
                let (answer, channel) = std::sync::mpsc::channel();
                let _ = answer.send(PortalOutcome::Cancelled);
                channel
            }
        }));
        let denied = Rc::new(Cell::new(false));
        let root = CustodyVpnRoot {
            child: Box::new(app),
            denied: denied.clone(),
        };
        let mut driver = custody_driver(Box::new(root), doc);

        arm.set(Some(ConnKind::WireGuard));
        driver.relayout();
        denied.set(true);
        driver.click_named("history-open").unwrap();
        let sealed = driver.pending_history_controls();
        assert_eq!(
            sealed.len(),
            1,
            "mounted custody saves the armed WireGuard import without traversal"
        );
        assert_eq!(sealed[0].invocation.address.resource, "vpn.vault");
        assert_eq!(sealed[0].invocation.address.action, "import-wireguard");
        assert!(sealed[0].invocation.arguments.is_empty());

        arm.set(Some(ConnKind::OpenVpn));
        driver.relayout();
        driver.click_named("history-open").unwrap();
        denied.set(false);
        driver.send(&WindowEvent::Focused(true));
        assert!(
            portal_attempts.borrow().is_empty(),
            "the saved import and the hidden OpenVPN import start no portal"
        );
        assert_eq!(
            driver.pending_history_controls().len(),
            1,
            "the hidden OpenVPN import is not saved"
        );

        // The import sheet, opened from the header and committed through its
        // published primary action.
        driver.click_named("vpn-import").unwrap();
        driver.relayout();
        let choose = published_bounds(&driver, "Choose file");
        driver.click_at(choose.center().x, choose.center().y);
        driver.relayout();
        assert!(
            portal_attempts.borrow().is_empty(),
            "the fresh import waits for ordinary confirmation"
        );
        let (field, instruction) = palette_control(&driver, "Confirmation");
        let value = instruction
            .as_deref()
            .and_then(|text| text.strip_prefix("Type "))
            .and_then(|text| text.strip_suffix(" to confirm"))
            .filter(|text| !text.is_empty())
            .expect("published confirmation field describes the required text");
        driver
            .perform_semantic_named(
                "action-palette",
                Some(&field),
                &ui::a11y::Action::SetValue(value.into()),
            )
            .unwrap();
        let (confirm, _) = palette_control(&driver, "Confirm");
        driver
            .perform_semantic_named(
                "action-palette",
                Some(&confirm),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        assert_eq!(
            portal_attempts.borrow().as_slice(),
            &[vec!["WireGuard (.conf)".to_owned(), "All files".to_owned()]],
            "the confirmed fresh WireGuard import opens the typed portal request once",
        );
        // The import slot is registered beside the window shell's own
        // action-bound controls.
        assert!(driver.control_coverage().registered >= 1);
    }

    #[test]
    fn control_custody_vpn_wireguard_openvpn_replace_one_and_fresh_after_drain() {
        let (mut app, _doc) = demo_with_doc();
        let target = app.vault.connections[0].id;
        app.provision_target = Some(target);
        app.queue_import(ConnKind::WireGuard);
        assert_eq!(app.provision_target, None);
        app.queue_import(ConnKind::OpenVpn);

        let replaced = ui::widget::controls::take_control_invocation(&mut app)
            .expect("OpenVPN replaces WireGuard");
        assert_eq!(
            replaced.address,
            ActionAddress::new("vpn.vault", "import-openvpn")
        );
        assert!(replaced.arguments.is_empty());
        assert!(ui::widget::controls::take_control_invocation(&mut app).is_none());

        app.queue_import(ConnKind::WireGuard);
        let fresh = ui::widget::controls::take_control_invocation(&mut app)
            .expect("fresh post-drain import");
        assert_eq!(
            fresh.address,
            ActionAddress::new("vpn.vault", "import-wireguard")
        );
        assert!(fresh.arguments.is_empty());
        assert!(ui::widget::controls::take_control_invocation(&mut app).is_none());
    }

    /// The demo build populated + read back — used by the tests that need a
    /// fleet to operate on. The live app never fabricates this.
    fn demo_with_doc() -> (VpnApp, AutoCommit) {
        let mut app = VpnApp::new_demo();
        let mut doc = AutoCommit::new();
        app.refresh(&mut doc);
        (app, doc)
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

    fn id_of(app: &VpnApp, name: &str) -> ConnId {
        app.vault
            .connections
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no connection {name}"))
            .id
    }

    #[test]
    fn a_demo_vault_is_published_by_connection_actions() {
        let (app, doc) = demo_with_doc();
        assert!(!VpnDoc::is_empty(&doc));
        let recorded = app.last_dispatched().expect("a connection action");
        let payload = String::from_utf8_lossy(&recorded.payload);
        assert!(
            payload.contains("AddConnection") || payload.contains("SetTorGlobal"),
            "{payload}"
        );
    }

    #[test]
    fn vpn_starts_empty_live_and_seeds_only_the_demo() {
        // Case `live_new_starts_empty_and_writes_nothing`.
        {
            // The honesty guard: the live constructor fabricates no tunnels, and
            // `refresh` on an empty document must not seed a fleet.
            let mut app = VpnApp::new();
            let mut doc = AutoCommit::new();
            app.refresh(&mut doc);
            assert!(app.vault.connections.is_empty(), "live app starts empty");
            assert!(
                VpnDoc::read_vault(&doc).connections.is_empty(),
                "an empty live app must not seed the shared document"
            );
        }
        // Case `demo_seeds_fleet_for_shots`.
        {
            // `--shots` still needs a populated frame.
            let (app, _doc) = demo_with_doc();
            assert_eq!(app.vault.connections.len(), 4);
            assert!(app.vault.selected.is_some());
        }
        // Case `typed_profile_context_selects_profile_and_config_tab_after_load`.
        {
            let mut app = VpnApp::new_demo();
            app.pending_tool_intent = Some(os_actions::ToolIntent::Vpn {
                route: os_actions::VpnRoute::Profile,
                profile: Some("Work".into()),
            });
            let mut doc = AutoCommit::new();
            app.reconcile(&mut doc);
            assert_eq!(app.vault.selected, Some(id_of(&app, "Work")));
            assert_eq!(app.detail_tab, DetailTab::Config);
            assert!(app.pending_tool_intent.is_none());
        }
        // Case `select_offset_wraps`.
        {
            let (mut app, _doc) = demo_with_doc();
            let first = app.vault.selected.unwrap();
            app.select_offset(-1);
            let last = app.vault.selected.unwrap();
            assert_ne!(first, last);
            app.select_offset(1);
            assert_eq!(app.vault.selected.unwrap(), first);
        }
    }

    #[test]
    fn the_connections_sit_beside_the_selected_one_and_push_on_a_phone() {
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
            let mut doc = AutoCommit::new();
            app.reconcile(&mut doc);
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
    fn keys_and_tabs_repoint_the_detail() {
        let mut app = framed(demo_with_doc(), 1280.0, 800.0);
        key(&mut app, "2");
        assert_eq!(app.detail_tab, DetailTab::Config);
        key(&mut app, "3");
        assert_eq!(app.detail_tab, DetailTab::Activity);
        let before = app.vault.selected;
        key(&mut app, "ArrowDown");
        assert_ne!(app.vault.selected, before, "the arrows move the selection");
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn vpn_connection_transitions_are_honest_and_dismissable() {
        // Case `confirm_sheet_is_the_shared_modal`:
        // The confirm is the shared `ModalDialog`: Escape dismisses it, and
        // a click on its primary button runs the real `confirm_pending`.
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
            let (mut app, _doc) = demo_with_doc();
            let id = app.vault.selected.expect("demo has a selected profile");
            app.vault
                .get_mut(id)
                .expect("selected profile exists")
                .status = ConnStatus::Connecting;

            app.request_toggle(id);

            assert_eq!(app.modal, Modal::None, "no second confirmation is opened");
            assert!(
                app.activity
                    .last()
                    .is_some_and(|line| line.contains("already changing state")),
                "the rejected request is visible in the local activity history"
            );
        }
        // Case `error_notice_can_be_dismissed_with_escape`.
        {
            let mut app = framed(demo_with_doc(), 1280.0, 800.0);
            app.operation_error = Some("The profile is incomplete".into());
            app.retry_import_kind = Some(ConnKind::WireGuard);
            Framed::harness_mut(&mut app).relayout();
            assert!(target(&app, "vpn-retry-import").width() > 0.0);

            key(&mut app, "Escape");

            assert!(app.operation_error.is_none());
            assert!(app.retry_import_kind.is_none());
        }
        // Case `confirm_connect_without_exec_stays_disconnected`.
        {
            // Honesty: with execution disabled (the default) confirming a connect
            // must NOT claim the tunnel is connected — the real command never ran.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_VPN_ALLOW_EXEC");
            let (mut app, mut doc) = demo_with_doc();
            let work = id_of(&app, "Work");
            assert!(!app.vault.get(work).unwrap().is_connected());
            app.select(work);
            app.request_toggle(work);
            assert_eq!(app.modal, Modal::Confirm);
            app.confirm_pending(&mut doc);
            let back = VpnDoc::read_vault(&doc);
            assert!(
                !back
                    .connections
                    .iter()
                    .find(|c| c.id == work)
                    .unwrap()
                    .is_connected(),
                "a refused command must never show as Connected"
            );
        }
        // Case `exported_tunnel_toggle_is_a_cancellable_background_job`.
        {
            let (mut app, mut doc) = demo_with_doc();
            let connection = app.vault.connections[0].id;
            let invocation = tilo_actions::InvocationId::new();
            let started = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    actions::VPN_TOGGLE_TYPE,
                    &serde_json::to_vec(&connection.to_string()).unwrap(),
                ),
                &mut doc,
                &automerge::ROOT,
            )
            .unwrap();
            assert_eq!(started, Some(ui::widget::ResourceOperationStart::Accepted));
            assert!(matches!(
                Widget::poll_resource_operation(&mut app, invocation, true, &doc, &automerge::ROOT),
                Some(tilo_actions::InvocationUpdate::Cancelled { .. })
            ));

            let action = ui::resources::resources(&app, &doc)
                .into_iter()
                .flat_map(|root| root.children)
                .flat_map(|connection| connection.actions)
                .find(|action| action.id == "connect" || action.id == "disconnect")
                .unwrap();
            assert!(matches!(
                action.contract.execution,
                tilo_actions::ExecutionMode::Background { cancellable: true }
            ));
            assert_eq!(action.contract.result, tilo_actions::ResultSpec::Job);
        }
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn vpn_import_and_setup_keep_secrets_out_of_the_document() {
        // Case `import_parses_metadata_without_duplicating_the_secret_file`.
        {
            // NetworkManager owns the imported profile and secrets. In a test
            // process host import is disabled, but the editor artifact is parsed
            // without retaining a second raw config/private-key file.
            let (mut app, mut doc) = demo_with_doc();
            let before = app.vault.connections.len();
            let raw = "[Interface]\nPrivateKey = c2VjcmV0\nAddress = 10.9.0.2/32\nDNS = 9.9.9.9\n\n[Peer]\nPublicKey = abc=\nEndpoint = imported.example:51820\nAllowedIPs = 0.0.0.0/0\n";
            let path = std::env::temp_dir().join(format!("home-{}.conf", uuid::Uuid::new_v4()));
            std::fs::write(&path, raw).unwrap();

            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            ui::host::scoped(&services, || app.request_import(ConnKind::WireGuard));
            assert_eq!(
                recorder.pick_requests()[0].title(),
                "Import WireGuard tunnel"
            );
            assert!(app.drain_import(&mut doc), "a delivered import applies");
            assert!(app.import_rx.is_none(), "draining clears the request");
            assert_eq!(app.vault.connections.len(), before + 1);

            let imported = app
                .vault
                .connections
                .iter()
                .find(|c| c.name.starts_with("home-"))
                .expect("imported secret-free profile identity is present")
                .clone();
            assert_eq!(imported.kind, ConnKind::WireGuard);
            assert!(
                !format!("{imported:?}").contains("c2VjcmV0"),
                "the private key never enters the collaborative model"
            );

            // The live plan uses the shared NetworkManager backend.
            let plan = config::connect_plan(&imported, config::default_backend(imported.kind));
            assert_eq!(plan.program, "nmcli");
            assert_eq!(plan.args, vec!["connection", "up", imported.name.as_str()]);

            let _ = std::fs::remove_file(&path);
        }
        // Case `exact_local_setup_keeps_the_shared_connection_id_and_document_secret_free`.
        {
            let (mut app, mut doc) = demo_with_doc();
            let id = app.vault.selected.expect("demo selection");
            let before = app.vault.connections.len();
            let raw = "[Interface]\nPrivateKey = setup-secret\nAddress = 10.9.0.2/32\n\n[Peer]\nPublicKey = peer=\nEndpoint = setup.example:51820\nAllowedIPs = 0.0.0.0/0\n";
            let path = std::env::temp_dir().join(format!("setup-{}.conf", uuid::Uuid::new_v4()));
            std::fs::write(&path, raw).unwrap();
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Opened(path.clone()));
            ui::host::scoped(&services, || app.request_local_provision(id));
            assert!(app.drain_import(&mut doc));
            assert_eq!(
                app.vault.connections.len(),
                before,
                "setup does not add a second shared profile"
            );
            assert!(
                app.vault.get(id).is_some(),
                "the original stable id remains"
            );
            assert!(
                app.last_dispatched().is_none(),
                "setup emits no AddConnection action"
            );
            assert!(!String::from_utf8_lossy(&doc.save()).contains("setup-secret"));
            assert!(!String::from_utf8_lossy(&doc.save()).contains("setup.example"));
            let _ = std::fs::remove_file(path);
        }
        // Case `cancelled_setup_cannot_capture_a_later_generic_import`.
        {
            let (mut app, _doc) = demo_with_doc();
            let id = app.vault.selected.expect("demo selection");
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.request_local_provision(id));
            let mut doc = AutoCommit::new();
            assert!(!app.drain_import(&mut doc));
            assert_eq!(app.provision_target, None);
            app.queue_import(ConnKind::OpenVpn);
            assert_eq!(
                app.provision_target, None,
                "generic import cannot inherit cancelled setup"
            );
        }
        // Case `unprovisioned_profile_resources_are_searchable_but_machine_actions_are_honest`.
        {
            let (mut app, doc) = demo_with_doc();
            app.stage_scenario(Some("unprovisioned"));
            let id = app.vault.selected.expect("selected remote profile");
            assert_eq!(id, id_of(&app, "Work"));
            let resources = ui::resources::resources(&app, &doc);
            let profile = resources[0]
                .children
                .iter()
                .find(|resource| resource.id.0 == format!("vpn.connection:{id}"))
                .expect("stable remote profile remains searchable");
            assert!(
                profile
                    .actions
                    .iter()
                    .any(|action| action.id == "provision-local")
            );
            for action_id in ["connect", "delete"] {
                let action = profile
                    .actions
                    .iter()
                    .find(|action| action.id == action_id)
                    .unwrap();
                assert!(
                    matches!(action.contract.availability, tilo_actions::Availability::Unavailable { ref code, .. } if code == "local_credentials_required")
                );
            }
            // Its main action is setting it up here, through the import sheet
            // fixed to the profile's protocol.
            app.main_action(id);
            app.refresh_sheet();
            assert_eq!(app.modal, Modal::Add);
            assert_eq!(app.provision_target, Some(id));
            assert_eq!(app.add_kind, ConnKind::OpenVpn);
        }
    }

    #[test]
    fn the_import_sheet_never_commits_an_unprovisioned_connection() {
        // Case `choosing_a_file_queues_the_typed_import`.
        {
            let (mut app, _doc) = demo_with_doc();
            let before = app.vault.connections.len();
            app.open_add(ConnKind::OpenVpn);
            app.commit_add();
            assert_eq!(app.vault.connections.len(), before);
            assert_eq!(app.modal, Modal::None);
            assert!(
                app.pending_control_invocation
                    .controlled_pending_for_test()
                    .is_some()
            );
        }
        // Case `the_header_button_opens_the_sheet_and_enter_chooses_a_file`.
        {
            let mut app = framed(demo_with_doc(), 1280.0, 800.0);
            let before = app.vault.connections.len();
            let import = target(&app, "vpn-import");
            click_at(&mut app, import.center());
            assert_eq!(app.modal, Modal::Add);
            assert!(app.dialog.as_ref().is_some_and(ModalDialog::is_open));

            // Typed text reaches no field: the sheet captures no credentials.
            Framed::harness_mut(&mut app).send(&WindowEvent::Ime(winit::event::Ime::Commit(
                "secret".into(),
            )));
            key(&mut app, "Enter");
            assert_eq!(app.modal, Modal::None);
            assert_eq!(app.vault.connections.len(), before);
            // The typed import went to the host for its ordinary confirmation.
            assert!(
                app.activity
                    .last()
                    .is_some_and(|line| line.contains("Waiting for approval"))
            );
            assert!(!app.activity.iter().any(|line| line.contains("secret")));
        }
        // Case `cancel_dismisses_without_saving`.
        {
            let mut app = framed(demo_with_doc(), 1280.0, 800.0);
            app.open_add(ConnKind::WireGuard);
            Framed::harness_mut(&mut app).relayout();
            let cancel = app.dialog.as_ref().unwrap().secondary_rect();
            assert!(cancel.width() > 0.0);
            click_at(&mut app, cancel.center());
            assert_eq!(app.modal, Modal::None, "Cancel dismisses without saving");
            assert!(app.dialog.is_none());
            assert!(
                app.pending_control_invocation
                    .controlled_pending_for_test()
                    .is_none()
            );
        }
    }
}
