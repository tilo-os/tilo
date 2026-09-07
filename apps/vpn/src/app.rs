//! Root widget for the VPN client.
//!
//! `VpnApp` is a single full-canvas [`Widget`] that owns the data flow:
//! it seeds + reads the [`Vault`] from the shared Automerge document,
//! lays out a connections list + detail pane + footer itself (no taffy
//! sub-tree), draws everything into the Vello scene, and routes its own
//! pointer/keyboard events back into CRDT mutations. Throughput is an
//! ephemeral, time-driven animation layered on top.

use std::collections::HashMap;
use std::time::Instant;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::cell::Cell;
use std::rc::Rc;
use ui::icons::Icon;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::qos_actions::{ActionAddress, ControlInvocation};
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, EmptyState, ModalDialog, Toggle, ToggleFace, Widget};
use zeroize::Zeroize;

use crate::actions::{self, VPN_ACTION_TYPE};
use crate::config::{self, CommandPlan};
use crate::doc::VpnDoc;
use crate::model::{ConnId, ConnKind, ConnStatus, Connection, Vault, human_ago, sample_vault};
use crate::net::{self, Transfer};
use crate::throughput;

/// One real byte-counter reading of a connected tunnel, timestamped so a
/// rate can be derived from consecutive samples. Ephemeral live telemetry —
/// never persisted to the CRDT, never fabricated.
#[derive(Clone, Copy, Debug)]
struct RateSample {
    at: Instant,
    xfer: Transfer,
}

/// Rolling window of real transfer readings for one tunnel.
#[derive(Default)]
struct TransferHistory {
    samples: Vec<RateSample>,
}

impl TransferHistory {
    /// Minimum spacing between kept samples (throttles the per-frame poll).
    const MIN_DT: f64 = 0.5;

    fn push(&mut self, xfer: Transfer) {
        let now = Instant::now();
        if let Some(last) = self.samples.last()
            && now.duration_since(last.at).as_secs_f64() < Self::MIN_DT
        {
            return;
        }
        self.samples.push(RateSample { at: now, xfer });
        // Keep enough points to fill the chart, plus one for the leading edge.
        let cap = throughput::SAMPLES + 1;
        if self.samples.len() > cap {
            let drop = self.samples.len() - cap;
            self.samples.drain(0..drop);
        }
    }

    /// Per-interval download/upload rate series in bytes/sec, one point per
    /// consecutive pair of readings. Empty until at least two real samples
    /// exist — we never invent a curve.
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

    fn has_rate(&self) -> bool {
        self.samples.len() >= 2
    }
}

const HEADER_H: f64 = 72.0;
const LIST_W: f64 = 440.0;

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
const ROW_H: f64 = 74.0;
const ROW_GAP: f64 = 8.0;

/// Which detail-pane tab is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailTab {
    Status,
    Config,
    Logs,
}
impl DetailTab {
    const ALL: [DetailTab; 3] = [DetailTab::Status, DetailTab::Config, DetailTab::Logs];
    fn label(self) -> &'static str {
        match self {
            DetailTab::Status => "Status",
            DetailTab::Config => "Config",
            DetailTab::Logs => "Logs",
        }
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

/// The live confirm dialog: the shared [`ModalDialog`] hosting a [`ConfirmBody`]
/// plus the confirm / cancel cells its footer buttons flip.
struct ConfirmDlg {
    dialog: ModalDialog<ConfirmBody>,
    confirm: Rc<Cell<bool>>,
    cancel: Rc<Cell<bool>>,
}

/// Content body for the confirm dialog: an action summary plus a preview box
/// showing the exact shell command that will run. The [`ModalDialog`] owns the
/// scrim, box, title, footer buttons, Esc, focus trap, and a11y role.
struct ConfirmBody {
    summary: String,
    cmd: String,
    rect: Rect,
}

impl ConfirmBody {
    fn new(summary: String, cmd: String) -> Self {
        Self {
            summary,
            cmd,
            rect: Rect::ZERO,
        }
    }
}

impl Widget for ConfirmBody {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let x = self.rect.x0;
        let width = self.rect.width().max(1.0) as f32;
        text_w(
            tr,
            scene,
            t,
            &self.summary,
            13.0,
            theme.text_dim,
            x,
            self.rect.y0,
            TextWeight::Regular,
        );
        let _ = width;
        // Command preview box below the summary.
        let box_top = self.rect.y0 + 36.0;
        let cmd_box = Rect::new(x, box_top, self.rect.x1, (box_top + 56.0).min(self.rect.y1));
        fill_round(
            scene,
            t,
            cmd_box,
            f64::from(theme.tokens.radii.md),
            theme.tokens.colors.neutral[0],
        );
        stroke_round(
            scene,
            t,
            cmd_box,
            f64::from(theme.tokens.radii.md),
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
            1.0,
        );
        text_w(
            tr,
            scene,
            t,
            "Will run:",
            10.0,
            theme.text_dim,
            cmd_box.x0 + 12.0,
            cmd_box.y0 + 8.0,
            TextWeight::Bold,
        );
        tr.draw_text_styled(
            scene,
            &ellipsize(&self.cmd, 52),
            13.0,
            theme.tokens.colors.semantic.success,
            t * Affine::translate((cmd_box.x0 + 12.0, cmd_box.y0 + 26.0)),
            None,
            TextWeight::Regular,
            false,
            true,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }

    fn name(&self) -> &'static str {
        "ConfirmBody"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "window focus, demo capture, compact navigation, and loading are orthogonal UI state"
)]
pub struct VpnApp {
    vault: Vault,
    /// Per-window selection is presentation state, not shared configuration.
    local_selected: Option<ConnId>,
    rect: Rect,
    node: Option<NodeId>,
    seeded: bool,
    detail_tab: DetailTab,
    modal: Modal,
    /// The live confirm dialog (shared [`ModalDialog`]), built lazily while
    /// `modal == Modal::Confirm` and torn down when it resolves.
    confirm_dialog: Option<ConfirmDlg>,
    add_kind: ConnKind,
    pending: Option<Pending>,
    /// A host-confirmed resource operation waiting for the next mutable-doc
    /// sync pass. The widget operation hook intentionally receives a read-only
    /// document; tunnel status is committed here after the real command runs.
    queued_resource_toggle: Option<(ConnId, qos_actions::InvocationId)>,
    completed_resource_toggle: Option<qos_actions::InvocationUpdate>,
    queued_delete: Option<(ConnId, String, qos_actions::InvocationId)>,
    completed_delete: Option<qos_actions::InvocationUpdate>,
    /// Most recent action results, newest last. Starts empty — populated
    /// only by real user actions (imports, connect attempts).
    activity: Vec<String>,
    /// In-flight XDG-portal file import, drained once per frame. `None` when
    /// idle.
    import_rx: Option<PortalChannel>,
    /// Which protocol the pending import was requested for (seeds the parser
    /// when the chosen file has an ambiguous extension).
    import_hint: ConnKind,
    import_invocation: Option<qos_actions::InvocationId>,
    completed_import: Option<qos_actions::InvocationUpdate>,
    pending_control_invocation: Option<ControlInvocation>,
    operation_error: Option<String>,
    /// Import errors can be retried without making the user rediscover which
    /// protocol they were importing. Other machine-action errors deliberately
    /// do not get a blind retry: the user can inspect the profile and choose
    /// Connect again after addressing the reported cause.
    retry_import_kind: Option<ConnKind>,
    last_mouse: Point,
    hovered_row: Option<ConnId>,
    is_focused: bool,
    /// Real, ephemeral throughput readings per connected tunnel (never in the
    /// CRDT). Populated only from live kernel counters when execution is
    /// allowed and the tunnel is actually up.
    throughput: HashMap<ConnId, TransferHistory>,
    /// Authoritative machine-local tunnel state. It is intentionally not
    /// written into the collaborative profile document.
    live_status: HashMap<ConnId, ConnStatus>,
    /// Last time the real interface state was reconciled, to throttle the
    /// per-frame `ip link` / `pgrep` probes to ~1 Hz.
    last_reconcile: Option<Instant>,
    /// Framework-owned action dispatch. Every document-mutating affordance (add
    /// a connection, toggle Tor) dispatches its [`crate::actions::VpnAction`]
    /// through this host, which brackets the undo boundary, encodes it, and
    /// applies it via the executor so it replays byte-identically on peers — the
    /// same path the Ctrl+K palette takes.
    actions: ui::ActionHost,
    /// One-shot context supplied by the shell's typed tool service. It is
    /// consumed only after the persisted vault has loaded, so a profile route
    /// can select the real NetworkManager-backed connection by name.
    pending_tool_intent: Option<os_actions::ToolIntent>,
    demo_mode: bool,
    compact_detail_open: bool,
    /// Per-window compact-detail scroll. This is presentation state and is
    /// never synchronized with the shared profile document.
    detail_scroll: f64,
    shot_loading: bool,
}

impl Default for VpnApp {
    fn default() -> Self {
        Self::new()
    }
}

impl VpnApp {
    /// The live app: starts empty. Only the user's real, persisted tunnels
    /// (held in the shared document) appear — no fabricated fleet.
    pub fn new() -> Self {
        let mut app = Self::build(Vault::default(), false);
        app.pending_tool_intent = os_actions::tool_intent_from_env();
        app
    }

    /// The hermetic `--shots` / demo build: seeded with a sample fleet so the
    /// screenshot is populated offline. This is the ONLY place sample tunnels
    /// come from — the live app never fabricates connections.
    pub fn new_demo() -> Self {
        Self::build(sample_vault(), true)
    }

    fn build(vault: Vault, demo_mode: bool) -> Self {
        let local_selected = vault.selected;
        let live_status = vault
            .connections
            .iter()
            .map(|connection| (connection.id, connection.status))
            .collect();
        VpnApp {
            vault,
            local_selected,
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            detail_tab: DetailTab::Status,
            modal: Modal::None,
            confirm_dialog: None,
            add_kind: ConnKind::WireGuard,
            pending: None,
            queued_resource_toggle: None,
            completed_resource_toggle: None,
            queued_delete: None,
            completed_delete: None,
            activity: Vec::new(),
            import_rx: None,
            import_hint: ConnKind::WireGuard,
            import_invocation: None,
            completed_import: None,
            pending_control_invocation: None,
            operation_error: None,
            retry_import_kind: None,
            last_mouse: Point::ZERO,
            hovered_row: None,
            is_focused: false,
            throughput: HashMap::new(),
            live_status,
            last_reconcile: None,
            actions: ui::ActionHost::new(),
            pending_tool_intent: None,
            demo_mode,
            compact_detail_open: false,
            detail_scroll: 0.0,
            shot_loading: false,
        }
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
    /// were built with — the user's real saved tunnels, or (demo build) the
    /// sample. An empty live vault publishes nothing, so the app stays honest.
    fn refresh(&mut self, doc: &mut AutoCommit) {
        if !self.seeded {
            if VpnDoc::is_empty(doc) && !self.vault.connections.is_empty() {
                VpnDoc::write_vault(doc, &self.vault);
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
                connection.last_used = "Active demo".into();
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

    /// Reconcile each tunnel's stored status against the REAL kernel/process
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
        let now = Instant::now();
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
            let status = if profiles
                .iter()
                .any(|profile| profile.name == connection.name && profile.active)
            {
                ConnStatus::Connected
            } else {
                ConnStatus::Disconnected
            };
            connection.status = status;
            self.live_status.insert(connection.id, status);
        }
    }

    /// Poll REAL byte counters for every connected tunnel and fold them into
    /// the ephemeral throughput history. No sample is ever synthesized: if the
    /// kernel has no counter for the interface, nothing is recorded. Disabled
    /// in hermetic tests and only runs for tunnels reporting Connected.
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
        self.detail_scroll = 0.0;
        if self.rect.width() < 720.0 {
            self.compact_detail_open = true;
        }
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

    /// Begin connecting/disconnecting a connection → raises a confirm.
    /// Seed the action confirm over the first connection for a deterministic
    /// `--shots` frame (no event loop).
    pub fn seed_shot_confirm(&mut self) {
        if let Some(id) = self.vault.connections.first().map(|c| c.id) {
            self.request_toggle(id);
        }
    }

    /// Open the shared-field Add form for a deterministic screenshot.
    pub fn seed_shot_add(&mut self) {
        self.open_add(ConnKind::WireGuard);
    }

    /// Configure a deterministic, hermetic visual-review state.
    pub fn seed_shot_state(&mut self, state: &str) {
        match state {
            "detail" => self.compact_detail_open = true,
            "config" => {
                self.compact_detail_open = true;
                self.detail_tab = DetailTab::Config;
            }
            "logs" => {
                self.compact_detail_open = true;
                self.detail_tab = DetailTab::Logs;
                self.activity
                    .push("NetworkManager: profile activation verified".into());
                self.activity
                    .push("No credentials were written to the shared document".into());
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
                    self.local_selected = Some(id);
                    self.vault.selected = Some(id);
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
                    self.local_selected = Some(id);
                    self.vault.selected = Some(id);
                    self.live_status.insert(id, ConnStatus::Connecting);
                }
            }
            "error" => {
                self.operation_error =
                    Some("The profile is incomplete: WireGuard Peer.PublicKey is missing.".into());
                self.retry_import_kind = Some(ConnKind::WireGuard);
            }
            "empty" => {
                self.vault = Vault::default();
                self.local_selected = None;
                self.live_status.clear();
            }
            _ => {}
        }
    }

    fn request_toggle(&mut self, id: ConnId) {
        let Some(c) = self.vault.get(id) else { return };
        if c.status == ConnStatus::Connecting {
            self.log(format!("{} is already changing state", c.name));
            return;
        }
        self.operation_error = None;
        self.retry_import_kind = None;
        let backend = config::default_backend(c.kind);
        self.pending = Some(if c.is_connected() {
            Pending::Disconnect(id, config::disconnect_plan(c, backend))
        } else {
            Pending::Connect(id, config::connect_plan(c, backend))
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
    /// **only if the real command actually succeeded** — the UI never claims
    /// a tunnel is connected on the strength of an optimistic guess. With
    /// `NetworkManager` remains the source of truth shared with quick settings.
    fn confirm_pending(&mut self, doc: &mut AutoCommit) {
        let Some(p) = self.pending.take() else { return };
        match p {
            Pending::Connect(id, plan) => match config::execute(&plan) {
                Ok(out) => {
                    let profile_name = self.vault.get(id).map(|c| c.name.clone());
                    let real_up = profile_name
                        .as_deref()
                        .map(config::network_manager_active)
                        .transpose();
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
                        qos_actions::ActionContext::for_action("org.qos.Vpn", "VPN", &action),
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
        self.import_rx = Some(tunnel_import_request(kind).send());
        self.log(format!("choose a .{} file to import…", kind.ext()));
    }

    fn queue_import(&mut self, kind: ConnKind) {
        let action = match kind {
            ConnKind::WireGuard => "import-wireguard",
            ConnKind::OpenVpn => "import-openvpn",
        };
        self.pending_control_invocation = Some(ControlInvocation {
            address: ActionAddress::new("vpn.vault", action),
            arguments: serde_json::Map::new(),
        });
    }

    /// Drain an in-flight import. When the user picked a file we read it,
    /// parse the real WireGuard/OpenVPN config into a [`Connection`], and add
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
                        let conn = config::parse_config(&stem, kind, &text);
                        // Hand the portal-selected file straight to NetworkManager.
                        // It becomes the sole owner of the profile and its secrets;
                        // Studio deliberately keeps no second raw private-key copy.
                        let nm_kind = match kind {
                            ConnKind::WireGuard => "wireguard",
                            ConnKind::OpenVpn => "openvpn",
                        };
                        let import_result = config::exec_allowed().then(|| {
                            os_actions::import_vpn_config(nm_kind, &path.to_string_lossy())
                        });
                        text.zeroize();
                        match import_result {
                            None => self.log(format!(
                                "parsed {} '{}' (host import disabled in test process)",
                                kind.badge(),
                                stem
                            )),
                            Some(Ok(())) => self.log(format!(
                                "imported {} '{}' into NetworkManager",
                                kind.badge(),
                                stem
                            )),
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
                        let action = actions::add_connection_action(conn);
                        self.actions.dispatch_with_context(
                            doc,
                            VPN_ACTION_TYPE,
                            &action,
                            qos_actions::ActionContext::for_action("org.qos.Vpn", "VPN", &action),
                        );
                        self.refresh(doc);
                        self.select(id);
                        self.operation_error = None;
                        self.finish_import_success();
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
                if let Some(invocation) = self.import_invocation.take() {
                    self.completed_import = Some(qos_actions::InvocationUpdate::Cancelled {
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
            self.completed_import = Some(qos_actions::InvocationUpdate::Succeeded {
                invocation,
                output: qos_actions::ActionOutput::default(),
            });
        }
    }

    fn finish_import_error(&mut self, message: String) {
        self.operation_error = Some(message.clone());
        self.retry_import_kind = Some(self.import_hint);
        self.log(message.clone());
        if let Some(invocation) = self.import_invocation.take() {
            self.completed_import = Some(qos_actions::InvocationUpdate::Failed {
                invocation,
                error: qos_actions::ActionFailure {
                    code: "vpn.import_failed".into(),
                    message,
                    details: None,
                    retryable: true,
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
            qos_actions::ActionContext::for_action("org.qos.Vpn", "VPN", &action),
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
        self.queue_import(self.add_kind);
        self.modal = Modal::None;
        self.log(format!(
            "waiting for approval to import a complete {} provider profile",
            self.add_kind.long_name()
        ));
    }

    /// The last [`qos_actions::EncodedAction`] an affordance dispatched, for the
    /// `affordance == ActionSpec` covenant test — an on-screen control and its
    /// Ctrl+K entry must build the byte-identical action.
    #[cfg(test)]
    pub(crate) fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    // ---- layout ------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "one geometry table is shared by paint, hit testing, and accessibility"
    )]
    fn layout(&self) -> LayoutRects {
        let r = self.rect;
        let compact = r.width() < 720.0;
        let base_header_h = if compact { 144.0 } else { HEADER_H };
        let has_notice =
            self.import_rx.is_some() || self.operation_error.is_some() || self.shot_loading;
        let header_h = base_header_h + if has_notice { 48.0 } else { 0.0 };
        let header = Rect::new(r.x0, r.y0, r.x1, r.y0 + header_h);
        let notice = has_notice.then_some(Rect::new(
            header.x0,
            header.y0 + base_header_h,
            header.x1,
            header.y1,
        ));
        // Recovery controls live inside the notice band. They are deliberately
        // large enough for touch and reserve their space before the message is
        // ellipsized, so an error can never paint underneath an action.
        let notice_dismiss =
            notice.map(|r| Rect::new(r.x1 - 16.0 - 68.0, r.y0 + 6.0, r.x1 - 16.0, r.y1 - 6.0));
        let notice_retry = notice.map(|r| {
            Rect::new(
                r.x1 - 16.0 - 68.0 - 8.0 - 78.0,
                r.y0 + 6.0,
                r.x1 - 16.0 - 76.0,
                r.y1 - 6.0,
            )
        });
        // No bottom chrome strip: the body spans to the window bottom.
        let body = Rect::new(r.x0, header.y1, r.x1, r.y1);
        let list = if compact {
            body
        } else {
            Rect::new(body.x0, body.y0, body.x0 + LIST_W, body.y1)
        };
        let detail = if compact {
            body
        } else {
            Rect::new(list.x1, body.y0, body.x1, body.y1)
        };

        // Header controls (right aligned), laid out right → left:
        // [ Import .conf ][ Import .ovpn ][ Tor pill ][ + Add tunnel ].
        let by0 = if compact {
            header.y0 + 52.0
        } else {
            header.y0 + 18.0
        };
        let by1 = by0 + 36.0;
        let (add_btn, tor_pill, imp_conf, imp_ovpn) = if compact {
            let left = header.x0 + 16.0;
            let right = header.x1 - 16.0;
            let gap = 8.0;
            let half = (right - left - gap) / 2.0;
            (
                Rect::new(left + half + gap, by0 + 44.0, right, by1 + 44.0),
                Rect::new(left, by0 + 44.0, left + half, by1 + 44.0),
                Rect::new(left, by0, left + half, by1),
                Rect::new(left + half + gap, by0, right, by1),
            )
        } else {
            let add = Rect::new(header.x1 - 24.0 - 132.0, by0, header.x1 - 24.0, by1);
            let tor = Rect::new(add.x0 - 12.0 - 150.0, by0, add.x0 - 12.0, by1);
            let ovpn = Rect::new(tor.x0 - 10.0 - 106.0, by0, tor.x0 - 10.0, by1);
            let conf = Rect::new(ovpn.x0 - 8.0 - 106.0, by0, ovpn.x0 - 8.0, by1);
            (add, tor, conf, ovpn)
        };

        // Rows.
        let mut rows = Vec::new();
        let mut y = list.y0 + 44.0; // leave room for the list section title
        for c in &self.vault.connections {
            let row = Rect::new(list.x0 + 12.0, y, list.x1 - 12.0, y + ROW_H);
            let toggle = Rect::new(
                row.x1 - 16.0 - 46.0,
                row.center().y - 13.0,
                row.x1 - 16.0,
                row.center().y + 13.0,
            );
            rows.push((c.id, row, toggle));
            y += ROW_H + ROW_GAP;
        }

        // Detail header + tabs + content.
        let dpad = 24.0;
        let detail_top = if compact { detail.y0 + 48.0 } else { detail.y0 };
        let dinner = Rect::new(
            detail.x0 + dpad,
            detail_top + dpad,
            detail.x1 - dpad,
            detail.y1 - dpad,
        );
        let connect_btn = if compact {
            Rect::new(dinner.x0, dinner.y0 + 62.0, dinner.x1, dinner.y0 + 104.0)
        } else {
            Rect::new(
                dinner.x1 - 150.0,
                dinner.y0 + 6.0,
                dinner.x1,
                dinner.y0 + 48.0,
            )
        };
        let tabs_y = dinner.y0 + if compact { 120.0 } else { 76.0 };
        let mut tabs = Vec::new();
        let mut tx = dinner.x0;
        for t in DetailTab::ALL {
            let w = if compact {
                (dinner.width() - 8.0) / 3.0
            } else {
                92.0
            };
            tabs.push((t, Rect::new(tx, tabs_y, tx + w, tabs_y + 34.0)));
            tx += w + 4.0;
        }
        let content = Rect::new(dinner.x0, tabs_y + 34.0 + 16.0, dinner.x1, dinner.y1);

        // Per-tunnel tor pill (in the Status tab kv-table) — computed on draw,
        // recomputed here for hit-testing.
        let tor_row = kv_tor_rect(content);

        // Modal card (centered).
        let modal_card = {
            let w = (r.width() - 24.0).min(480.0);
            // Two labelled 34px fields plus the import tip and button row do
            // not fit in 300px without the tip overlapping the endpoint.
            let h = 330.0;
            let cx = r.center().x;
            let cy = r.center().y;
            Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
        };

        LayoutRects {
            compact,
            header,
            list,
            detail,
            add_btn,
            tor_pill,
            rows,
            connect_btn,
            tabs,
            content,
            tor_row,
            imp_conf,
            imp_ovpn,
            modal_card,
            back_btn: Rect::new(
                detail.x0 + 16.0,
                detail.y0 + 10.0,
                detail.x0 + 112.0,
                detail.y0 + 42.0,
            ),
            notice,
            notice_retry,
            notice_dismiss,
        }
    }
}

struct LayoutRects {
    compact: bool,
    header: Rect,
    list: Rect,
    detail: Rect,
    add_btn: Rect,
    tor_pill: Rect,
    rows: Vec<(ConnId, Rect, Rect)>,
    connect_btn: Rect,
    tabs: Vec<(DetailTab, Rect)>,
    content: Rect,
    tor_row: Rect,
    imp_conf: Rect,
    imp_ovpn: Rect,
    modal_card: Rect,
    back_btn: Rect,
    notice: Option<Rect>,
    notice_retry: Option<Rect>,
    notice_dismiss: Option<Rect>,
}

/// Position of the per-tunnel-Tor pill inside the Status kv table.
fn kv_tor_rect(content: Rect) -> Rect {
    // 5th kv row; rows are 30px tall starting after the chart.
    let kv_top = content.y0 + 72.0 + 16.0 + 176.0 + 16.0;
    let row_y = kv_top + 4.0 * 30.0;
    Rect::new(
        content.x1 - 84.0,
        row_y + 2.0,
        content.x1 - 84.0 + 64.0,
        row_y + 26.0,
    )
}

fn point_in(r: Rect, p: Point) -> bool {
    p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
}

// ----------------------------------------------------------------------
// Drawing primitives
// ----------------------------------------------------------------------

fn fill_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color) {
    scene.fill(
        Fill::NonZero,
        t,
        color,
        None,
        &RoundedRect::from_rect(r, radius),
    );
}
fn stroke_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color, w: f64) {
    scene.stroke(
        &Stroke::new(w),
        t,
        color,
        None,
        &RoundedRect::from_rect(r, radius),
    );
}

#[allow(clippy::too_many_arguments)]
fn text_w(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    top: f64,
    weight: TextWeight,
) {
    tr.draw_text_weighted(
        scene,
        s,
        size,
        color,
        t * Affine::translate((x, top)),
        None,
        weight,
    );
}

/// Center text vertically within a box and left-align at `x`.
#[allow(clippy::too_many_arguments)]
fn text_vc(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    x: f64,
    box_y0: f64,
    box_h: f64,
    weight: TextWeight,
) {
    let top = box_y0 + (box_h - f64::from(size)) / 2.0;
    text_w(tr, scene, t, s, size, color, x, top, weight);
}

/// Center text horizontally + vertically within a rect (button labels).
#[allow(
    clippy::too_many_arguments,
    reason = "thin positional wrapper over text_w's draw call; grouping into a struct would only move the fields, not reduce them"
)]
fn text_center(
    tr: &mut TextRenderer,
    scene: &mut Scene,
    t: Affine,
    s: &str,
    size: f32,
    color: Color,
    r: Rect,
    weight: TextWeight,
) {
    let (tw, _) = tr.measure_weighted(s, size, None, weight);
    let x = r.center().x - f64::from(tw) / 2.0;
    let top = r.y0 + (r.height() - f64::from(size)) / 2.0;
    text_w(tr, scene, t, s, size, color, x, top, weight);
}

/// Truncate a string to a max display width by characters.
fn ellipsize(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

impl VpnApp {
    fn status_color(theme: &Theme, status: ConnStatus) -> Color {
        match status {
            ConnStatus::Connected => theme.tokens.colors.semantic.success,
            ConnStatus::Connecting => theme.tokens.colors.semantic.warning,
            ConnStatus::Disconnected => theme.tokens.colors.neutral[5],
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a badge is scene + renderer + transform + theme + position + protocol; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_badge(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        x: f64,
        cy: f64,
        kind: ConnKind,
    ) -> f64 {
        let label = kind.badge();
        let (w, _) = tr.measure_weighted(label, 11.0, None, TextWeight::Bold);
        let pad = 8.0;
        let bw = f64::from(w) + pad * 2.0;
        let r = Rect::new(x, cy - 9.0, x + bw, cy + 9.0);
        let bg = match kind {
            ConnKind::WireGuard => theme.tokens.colors.accent[5].multiply_alpha(0.22),
            ConnKind::OpenVpn => theme.tokens.colors.semantic.info.multiply_alpha(0.20),
        };
        let fg = match kind {
            ConnKind::WireGuard => theme.tokens.colors.accent[6],
            ConnKind::OpenVpn => theme.tokens.colors.semantic.info,
        };
        fill_round(scene, t, r, 9.0, bg);
        text_center(tr, scene, t, label, 11.0, fg, r, TextWeight::Bold);
        bw
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the header and optional live-status band are one immediate-mode paint boundary"
    )]
    fn draw_header(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
    ) {
        let h = lay.header;
        fill_round(
            scene,
            t,
            Rect::new(h.x0, h.y0, h.x1, h.y1),
            0.0,
            theme.tokens.colors.surfaces.raised,
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
            None,
            &Line::new(Point::new(h.x0, h.y1), Point::new(h.x1, h.y1)),
        );
        // Primary left label: the contextual saved/active tally. The compositor
        // owns window chrome, so the app draws no wordmark or brand mark here.
        let sub = format!(
            "{} saved · {} active",
            self.vault.connections.len(),
            self.vault.active_count()
        );
        text_vc(
            tr,
            scene,
            t,
            &sub,
            14.0,
            theme.text_primary,
            h.x0 + 24.0,
            h.y0,
            if lay.compact { 44.0 } else { HEADER_H },
            TextWeight::Semibold,
        );

        // Compact import buttons.
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(lay.imp_conf, "Import .conf")
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(12.0)
                .hovered(point_in(lay.imp_conf, self.last_mouse)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(lay.imp_ovpn, "Import .ovpn")
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(12.0)
                .hovered(point_in(lay.imp_ovpn, self.last_mouse)),
        );

        // Tor global pill.
        Self::draw_pill_toggle(
            scene,
            tr,
            t,
            theme,
            lay.tor_pill,
            "Tor",
            self.vault.tor_global,
        );

        // Add button (accent).
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(lay.add_btn, "+ Import")
                .primary()
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(13.0)
                .weight(TextWeight::Semibold)
                .hovered(point_in(lay.add_btn, self.last_mouse)),
        );

        if let Some(notice) = lay.notice {
            let (label, color) = if let Some(error) = &self.operation_error {
                (
                    format!("Action needed · {error}"),
                    theme.tokens.colors.semantic.danger,
                )
            } else {
                (
                    format!(
                        "Waiting for a .{} profile from the file picker…",
                        self.import_hint.ext()
                    ),
                    theme.tokens.colors.semantic.info,
                )
            };
            scene.fill(Fill::NonZero, t, color.multiply_alpha(0.10), None, &notice);
            let has_recovery = self.operation_error.is_some();
            let text_right = if has_recovery {
                lay.notice_retry
                    .filter(|_| self.retry_import_kind.is_some())
                    .map_or_else(
                        || lay.notice_dismiss.map_or(notice.x1, |r| r.x0 - 10.0),
                        |r| r.x0 - 10.0,
                    )
            } else {
                notice.x1 - 24.0
            };
            let message_chars = ((text_right - notice.x0 - 24.0) / 6.5).max(12.0).floor() as usize;
            text_vc(
                tr,
                scene,
                t,
                &ellipsize(&label, message_chars),
                12.0,
                color,
                notice.x0 + 24.0,
                notice.y0,
                notice.height(),
                TextWeight::Semibold,
            );
            if has_recovery {
                if let (Some(retry), Some(_)) = (lay.notice_retry, self.retry_import_kind) {
                    Button::paint(
                        scene,
                        t,
                        tr,
                        theme,
                        &ButtonFace::new(retry, "Retry")
                            .radius(f64::from(theme.tokens.radii.md))
                            .text_size(11.0)
                            .hovered(point_in(retry, self.last_mouse)),
                    );
                }
                if let Some(dismiss) = lay.notice_dismiss {
                    Button::paint(
                        scene,
                        t,
                        tr,
                        theme,
                        &ButtonFace::new(dismiss, "Dismiss")
                            .radius(f64::from(theme.tokens.radii.md))
                            .text_size(11.0)
                            .hovered(point_in(dismiss, self.last_mouse)),
                    );
                }
            }
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "a pill toggle is scene + renderer + transform + theme + rect + label + state; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_pill_toggle(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        r: Rect,
        label: &str,
        on: bool,
    ) {
        let bg = if on {
            theme.tokens.colors.accent[5].multiply_alpha(0.22)
        } else {
            theme.tokens.colors.surfaces.base
        };
        fill_round(scene, t, r, f64::from(theme.tokens.radii.full), bg);
        stroke_round(
            scene,
            t,
            r,
            f64::from(theme.tokens.radii.full),
            if on {
                theme.tokens.colors.accent[5]
            } else {
                theme.tokens.colors.border_standard
            },
            1.0,
        );
        // status dot
        let dot: Color = if on {
            theme.tokens.colors.accent[6]
        } else {
            theme.tokens.colors.neutral[5]
        };
        scene.fill(
            Fill::NonZero,
            t,
            dot,
            None,
            &Circle::new(Point::new(r.x0 + 18.0, r.center().y), 5.0),
        );
        let txt = format!("{label}  {}", if on { "On" } else { "Off" });
        text_vc(
            tr,
            scene,
            t,
            &txt,
            12.0,
            theme.text_primary,
            r.x0 + 32.0,
            r.y0,
            r.height(),
            TextWeight::Semibold,
        );
    }

    fn draw_list(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
    ) {
        let l = lay.list;
        // pane bg
        fill_round(scene, t, l, 0.0, theme.tokens.colors.surfaces.base);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
            None,
            &Line::new(Point::new(l.x1, l.y0), Point::new(l.x1, l.y1)),
        );
        text_w(
            tr,
            scene,
            t,
            "CONNECTIONS",
            11.0,
            theme.text_dim,
            l.x0 + 16.0,
            l.y0 + 18.0,
            TextWeight::Bold,
        );

        for (id, row, toggle) in &lay.rows {
            let Some(c) = self.vault.get(*id) else {
                continue;
            };
            let selected = self.vault.selected == Some(*id);
            let hovered = self.hovered_row == Some(*id);
            Self::draw_list_row(scene, tr, t, theme, c, row, toggle, selected, hovered);
        }
    }

    /// One connection row: selection/hover background, status dot, name +
    /// protocol badge, endpoint/last-used line, the per-tunnel Tor flag, and
    /// the connect toggle switch. Split out of [`Self::draw_list`] to keep
    /// that function's line count in check.
    #[allow(
        clippy::too_many_arguments,
        reason = "one list row is scene + renderer + transform + theme + the connection + its two laid-out rects + selection/hover state; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_list_row(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        c: &Connection,
        row: &Rect,
        toggle: &Rect,
        selected: bool,
        hovered: bool,
    ) {
        let radius = f64::from(theme.tokens.radii.lg);
        if selected {
            fill_round(
                scene,
                t,
                *row,
                radius,
                theme.tokens.colors.accent[5].multiply_alpha(0.16),
            );
            stroke_round(
                scene,
                t,
                *row,
                radius,
                theme.tokens.colors.accent[5].multiply_alpha(0.7),
                1.0,
            );
        } else if hovered {
            fill_round(scene, t, *row, radius, theme.tokens.colors.lift(0.35));
        }

        // status dot
        let sc = Self::status_color(theme, c.status);
        scene.fill(
            Fill::NonZero,
            t,
            sc,
            None,
            &Circle::new(Point::new(row.x0 + 22.0, row.y0 + 24.0), 5.0),
        );
        if c.is_connected() {
            scene.stroke(
                &Stroke::new(1.5),
                t,
                sc.multiply_alpha(0.4),
                None,
                &Circle::new(Point::new(row.x0 + 22.0, row.y0 + 24.0), 9.0),
            );
        }

        // name + badge
        let name = ellipsize(&c.name, 18);
        text_w(
            tr,
            scene,
            t,
            &name,
            15.0,
            theme.text_primary,
            row.x0 + 38.0,
            row.y0 + 14.0,
            TextWeight::Semibold,
        );
        let (nw, _) = tr.measure_weighted(&name, 15.0, None, TextWeight::Semibold);
        Self::draw_badge(
            scene,
            tr,
            t,
            theme,
            row.x0 + 38.0 + f64::from(nw) + 10.0,
            row.y0 + 24.0,
            c.kind,
        );

        // endpoint + last used
        let line2 = format!("{} · {}", ellipsize(&c.endpoint, 28), c.last_used);
        text_w(
            tr,
            scene,
            t,
            &line2,
            12.0,
            theme.text_dim,
            row.x0 + 38.0,
            row.y0 + 44.0,
            TextWeight::Regular,
        );

        // per-tunnel tor flag mark
        if c.tor {
            text_w(
                tr,
                scene,
                t,
                "tor",
                10.0,
                theme.tokens.colors.accent[6],
                toggle.x0 - 36.0,
                row.y0 + 14.0,
                TextWeight::Bold,
            );
        }

        // toggle switch
        Self::draw_switch(scene, t, theme, *toggle, c.is_connected());
    }

    fn draw_switch(scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, on: bool) {
        Toggle::paint(
            scene,
            t,
            theme,
            &ToggleFace::new(r, on).on_color(theme.tokens.colors.semantic.success),
        );
    }

    fn draw_detail(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
    ) {
        let d = lay.detail;
        fill_round(scene, t, d, 0.0, theme.tokens.colors.surfaces.base);

        if lay.compact {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(lay.back_btn, "← Connections")
                    .radius(f64::from(theme.tokens.radii.md))
                    .text_size(12.0)
                    .hovered(point_in(lay.back_btn, self.last_mouse)),
            );
        }

        let Some(c) = self.vault.selected_conn() else {
            self.draw_detail_empty_state(scene, tr, t, theme, d);
            return;
        };
        let detail_top = if lay.compact { d.y0 + 48.0 } else { d.y0 };
        let dinner = Rect::new(d.x0 + 24.0, detail_top + 24.0, d.x1 - 24.0, d.y1 - 24.0);

        Self::draw_detail_header(scene, tr, t, theme, dinner, c);
        self.draw_detail_connect_button(scene, tr, t, theme, lay, c);
        self.draw_detail_tabs(scene, tr, t, theme, lay);

        let content_transform = if lay.compact {
            scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &lay.content);
            t * Affine::translate((0.0, -self.detail_scroll))
        } else {
            t
        };
        match self.detail_tab {
            DetailTab::Status => {
                self.draw_status_tab(scene, tr, content_transform, theme, lay, c);
            }
            DetailTab::Config => {
                Self::draw_config_tab(scene, tr, content_transform, theme, lay, c);
            }
            DetailTab::Logs => {
                self.draw_logs_tab(scene, tr, content_transform, theme, lay, c);
            }
        }
        if lay.compact {
            scene.pop_layer();
        }
    }

    /// Composed zero-state centered in the detail pane. Distinguishes "no
    /// tunnels yet" (a real onboarding prompt) from the transient "nothing
    /// selected" case. The add + import affordances already live in the
    /// header, so this stays a non-interactive prompt. Split out of
    /// [`Self::draw_detail`] to keep that function's line count in check.
    fn draw_detail_empty_state(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        d: Rect,
    ) {
        let (title, message): (&str, &str) = if self.vault.connections.is_empty() {
            (
                "No VPN connections",
                "Import a complete .conf or .ovpn provider profile to connect.",
            )
        } else {
            (
                "Select a connection",
                "Choose a tunnel from the list to see its status and controls.",
            )
        };
        EmptyState::paint(
            scene,
            t,
            tr,
            theme,
            d,
            Some(Icon::Shield),
            title,
            Some(message),
            None,
            Point::ZERO,
        );
    }

    /// The title, status pill and endpoint subtitle at the top of the detail
    /// pane. Split out of [`Self::draw_detail`].
    fn draw_detail_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        dinner: Rect,
        c: &Connection,
    ) {
        text_w(
            tr,
            scene,
            t,
            &c.name,
            24.0,
            theme.text_primary,
            dinner.x0,
            dinner.y0,
            TextWeight::Bold,
        );
        let (tw, _) = tr.measure_weighted(&c.name, 24.0, None, TextWeight::Bold);
        // status pill
        let sc = Self::status_color(theme, c.status);
        let pill_x = dinner.x0 + f64::from(tw) + 14.0;
        let plabel = c.status.label();
        let (pw, _) = tr.measure_weighted(plabel, 12.0, None, TextWeight::Semibold);
        let pill = Rect::new(
            pill_x,
            dinner.y0 + 2.0,
            pill_x + f64::from(pw) + 28.0,
            dinner.y0 + 26.0,
        );
        fill_round(
            scene,
            t,
            pill,
            f64::from(theme.tokens.radii.full),
            sc.multiply_alpha(0.18),
        );
        scene.fill(
            Fill::NonZero,
            t,
            sc,
            None,
            &Circle::new(Point::new(pill.x0 + 12.0, pill.center().y), 4.0),
        );
        text_vc(
            tr,
            scene,
            t,
            plabel,
            12.0,
            sc,
            pill.x0 + 22.0,
            pill.y0,
            pill.height(),
            TextWeight::Semibold,
        );

        let sub = format!("{} · {}", c.kind.long_name(), ellipsize(&c.endpoint, 40));
        text_w(
            tr,
            scene,
            t,
            &sub,
            13.0,
            theme.text_dim,
            dinner.x0,
            dinner.y0 + 34.0,
            TextWeight::Regular,
        );
    }

    /// The Connect/Disconnect button, coloured and labelled from the
    /// connection's live status. Split out of [`Self::draw_detail`].
    fn draw_detail_connect_button(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
        c: &Connection,
    ) {
        let connecting = c.status == ConnStatus::Connecting;
        let face = if connecting {
            ButtonFace::new(lay.connect_btn, "Connecting…").disabled(true)
        } else if c.is_connected() {
            ButtonFace::new(lay.connect_btn, "Disconnect").danger()
        } else {
            ButtonFace::new(lay.connect_btn, "Connect").primary()
        };
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &face
                .radius(f64::from(theme.tokens.radii.md))
                .weight(TextWeight::Semibold)
                .disabled(connecting)
                .hovered(point_in(lay.connect_btn, self.last_mouse)),
        );
    }

    /// The Status/Config/Logs tab strip. Split out of [`Self::draw_detail`].
    fn draw_detail_tabs(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
    ) {
        for (tab, r) in &lay.tabs {
            let active = *tab == self.detail_tab;
            if active {
                fill_round(
                    scene,
                    t,
                    *r,
                    f64::from(theme.tokens.radii.md),
                    theme.tokens.colors.surfaces.raised,
                );
                stroke_round(
                    scene,
                    t,
                    *r,
                    f64::from(theme.tokens.radii.md),
                    ui::tokens::colors::Colors::hairline_on(
                        theme.tokens.colors.surfaces.raised,
                        26,
                    ),
                    1.0,
                );
            }
            let col = if active {
                theme.text_primary
            } else {
                theme.text_dim
            };
            text_center(
                tr,
                scene,
                t,
                tab.label(),
                13.0,
                col,
                *r,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
            );
        }
    }

    fn draw_status_tab(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
        c: &Connection,
    ) {
        let content = lay.content;

        // Real throughput, read from the kernel (`wg show transfer` / sysfs)
        // for this tunnel. `None`/empty until at least two real samples exist;
        // no value is ever synthesized.
        let hist = self.throughput.get(&c.id);
        let (down_series, up_series) = hist.map(TransferHistory::rate_series).unwrap_or_default();
        let has_rate = hist.is_some_and(TransferHistory::has_rate) && c.is_connected();
        let latest = hist.and_then(TransferHistory::latest);
        let down_now = down_series.last().copied();
        let up_now = up_series.last().copied();
        let tiles_h = if content.width() < 500.0 { 120.0 } else { 72.0 };

        Self::draw_status_tiles(
            scene, tr, t, theme, content, c, has_rate, down_now, up_now, tiles_h,
        );
        let chart = Self::draw_status_throughput(
            scene,
            tr,
            t,
            theme,
            content,
            tiles_h,
            &down_series,
            &up_series,
            has_rate,
            latest,
            c,
        );
        Self::draw_status_kv_rows(scene, tr, t, theme, content, chart.y1 + 16.0, lay, c);
    }

    /// The four-across stat tiles (assigned IP / handshake age / measured
    /// down / measured up). Split out of [`Self::draw_status_tab`] to keep
    /// that function's line count in check.
    #[allow(
        clippy::too_many_arguments,
        reason = "a stat-tile row is scene + renderer + transform + theme + layout + the connection + the already-computed rate values; grouping into a struct would only move the fields, not reduce them"
    )]
    #[allow(
        clippy::many_single_char_names,
        clippy::similar_names,
        reason = "compact geometry uses conventional x/y/rect names beside the total tile height"
    )]
    fn draw_status_tiles(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        content: Rect,
        c: &Connection,
        has_rate: bool,
        down_now: Option<f64>,
        up_now: Option<f64>,
        tiles_h: f64,
    ) {
        // Stat tiles (4 across). DOWN/UP show the measured current rate when we
        // have real samples, otherwise "—". Handshake reflects the real
        // last-handshake age when connected.
        let compact = content.width() < 500.0;
        let gap = if compact { 8.0 } else { 12.0 };
        let columns = if compact { 2.0 } else { 4.0 };
        let tw = (content.width() - gap * (columns - 1.0)) / columns;
        let rate_or_dash = |v: Option<f64>| match v {
            Some(bps) if has_rate => throughput::rate_label(bps),
            _ => "—".to_string(),
        };
        let tile_vals = [
            (
                "ASSIGNED IP",
                if c.address.is_empty() {
                    "—".to_string()
                } else {
                    c.address.clone()
                },
            ),
            (
                "HANDSHAKE",
                match c.last_handshake_secs {
                    Some(s) if c.is_connected() => human_ago(s),
                    _ => "—".to_string(),
                },
            ),
            ("DOWN", rate_or_dash(down_now)),
            ("UP", rate_or_dash(up_now)),
        ];
        for (i, (label, val)) in tile_vals.iter().enumerate() {
            let column = if compact { i % 2 } else { i };
            let row = if compact { i / 2 } else { 0 };
            let tile_h = if compact { 56.0 } else { tiles_h };
            let x = content.x0 + column as f64 * (tw + gap);
            let y = content.y0 + row as f64 * (tile_h + gap);
            let r = Rect::new(x, y, x + tw, y + tile_h);
            fill_round(
                scene,
                t,
                r,
                f64::from(theme.tokens.radii.lg),
                theme.tokens.colors.surfaces.raised,
            );
            stroke_round(
                scene,
                t,
                r,
                f64::from(theme.tokens.radii.lg),
                ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
                1.0,
            );
            text_w(
                tr,
                scene,
                t,
                label,
                10.0,
                theme.text_dim,
                r.x0 + 14.0,
                r.y0 + if compact { 8.0 } else { 14.0 },
                TextWeight::Bold,
            );
            text_w(
                tr,
                scene,
                t,
                &ellipsize(val, 16),
                17.0,
                theme.text_primary,
                r.x0 + 14.0,
                r.y0 + if compact { 27.0 } else { 36.0 },
                TextWeight::Semibold,
            );
        }
    }

    /// The real measured-rate sparkline chart plus its legend/empty note.
    /// Returns the chart's rect so the caller can position what follows it.
    /// Split out of [`Self::draw_status_tab`].
    #[allow(
        clippy::too_many_arguments,
        reason = "the throughput panel is scene + renderer + transform + theme + layout + both rate series + the already-computed rate state; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_status_throughput(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        content: Rect,
        tiles_h: f64,
        down_series: &[f64],
        up_series: &[f64],
        has_rate: bool,
        latest: Option<Transfer>,
        c: &Connection,
    ) -> Rect {
        // Throughput panel — plots the REAL measured rate series. When there
        // is no live data yet the plot stays empty with an honest note; the
        // app never draws a synthesized waveform.
        let chart = Rect::new(
            content.x0,
            content.y0 + tiles_h + 16.0,
            content.x1,
            content.y0 + tiles_h + 16.0 + if content.width() < 500.0 { 96.0 } else { 176.0 },
        );
        throughput::draw_chart(scene, t, chart, theme, down_series, up_series, has_rate);
        text_w(
            tr,
            scene,
            t,
            "Throughput",
            12.0,
            theme.text_dim,
            chart.x0 + 12.0,
            chart.y0 + 10.0,
            TextWeight::Bold,
        );
        if let Some(x) = latest {
            // Real cumulative counters straight from the kernel.
            let legend = format!(
                "recv {} · sent {}",
                ui::format::bytes(x.rx),
                ui::format::bytes(x.tx)
            );
            let (lw, _) = tr.measure_weighted(&legend, 11.0, None, TextWeight::Regular);
            text_w(
                tr,
                scene,
                t,
                &legend,
                11.0,
                theme.text_dim,
                chart.x1 - 12.0 - f64::from(lw),
                chart.y0 + 10.0,
                TextWeight::Regular,
            );
        }
        if !has_rate {
            let note = if !c.is_connected() {
                "Connect to read live throughput"
            } else if !config::exec_allowed() {
                "Live metering is unavailable in this test session"
            } else if latest.is_some() {
                "Measuring…"
            } else {
                "No kernel counters for this interface"
            };
            let (nw, _) = tr.measure_weighted(note, 12.0, None, TextWeight::Regular);
            text_w(
                tr,
                scene,
                t,
                note,
                12.0,
                theme.text_dim,
                chart.center().x - f64::from(nw) / 2.0,
                chart.center().y - 8.0,
                TextWeight::Regular,
            );
        }
        chart
    }

    /// The key/value table (endpoint, DNS, allowed IPs, public key) plus the
    /// per-tunnel Tor toggle row. Split out of [`Self::draw_status_tab`].
    #[allow(
        clippy::too_many_arguments,
        reason = "the kv table is scene + renderer + transform + theme + layout + the top y-offset + the connection; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_status_kv_rows(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        content: Rect,
        kv_top: f64,
        lay: &LayoutRects,
        c: &Connection,
    ) {
        let max_chars = if content.width() < 500.0 { 27 } else { 40 };
        let rows = [
            ("Endpoint", ellipsize(&c.endpoint, max_chars)),
            ("DNS", ellipsize(&c.dns, max_chars)),
            ("Allowed IPs", ellipsize(&c.allowed_ips, max_chars)),
            ("Public key", ellipsize(&c.public_key, max_chars)),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = kv_top + i as f64 * 30.0;
            text_w(
                tr,
                scene,
                t,
                k,
                12.0,
                theme.text_dim,
                content.x0,
                ry + 6.0,
                TextWeight::Regular,
            );
            text_w(
                tr,
                scene,
                t,
                v,
                12.0,
                theme.text_primary,
                content.x0 + 130.0,
                ry + 6.0,
                TextWeight::Regular,
            );
            scene.stroke(
                &Stroke::new(1.0),
                t,
                ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26)
                    .multiply_alpha(0.5),
                None,
                &Line::new(
                    Point::new(content.x0, ry + 30.0),
                    Point::new(content.x1, ry + 30.0),
                ),
            );
        }
        // Per-tunnel Tor row (with a small toggle).
        let ry = kv_top + 4.0 * 30.0;
        text_w(
            tr,
            scene,
            t,
            "Route over Tor",
            12.0,
            theme.text_dim,
            content.x0,
            ry + 6.0,
            TextWeight::Regular,
        );
        Self::draw_switch(scene, t, theme, lay.tor_row, c.tor);
    }

    fn draw_config_tab(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
        c: &Connection,
    ) {
        let content = lay.content;
        let card = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y1.min(content.y0 + 360.0),
        );
        fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.neutral[0],
        );
        stroke_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
            1.0,
        );
        let text = render_config_text(c);
        let mut y = card.y0 + 14.0;
        for line in text.lines() {
            let col = if line.starts_with('[') || line.starts_with('<') {
                theme.tokens.colors.accent[6]
            } else if line.contains('=') || line.split_whitespace().count() > 1 {
                theme.text_primary
            } else {
                theme.text_dim
            };
            tr.draw_text_styled(
                scene,
                line,
                12.5,
                col,
                t * Affine::translate((card.x0 + 16.0, y)),
                None,
                TextWeight::Regular,
                false,
                true,
            );
            y += 19.0;
            if y > card.y1 - 16.0 {
                break;
            }
        }
    }

    fn draw_logs_tab(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        lay: &LayoutRects,
        c: &Connection,
    ) {
        let content = lay.content;
        let card = Rect::new(
            content.x0,
            content.y0,
            content.x1,
            content.y1.min(content.y0 + 360.0),
        );
        fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.neutral[0],
        );
        stroke_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 26),
            1.0,
        );
        let xfer = self.throughput.get(&c.id).and_then(TransferHistory::latest);
        let lines = synth_logs(c, &self.activity, xfer);
        let mut y = card.y0 + 14.0;
        for line in &lines {
            let col =
                if line.contains("error") || line.contains("denied") || line.contains("disabled") {
                    theme.tokens.colors.semantic.warning
                } else if line.contains("ok") || line.contains("up") || line.contains("handshake") {
                    theme.tokens.colors.semantic.success
                } else {
                    theme.text_dim
                };
            tr.draw_text_styled(
                scene,
                &ellipsize(line, 78),
                12.0,
                col,
                t * Affine::translate((card.x0 + 16.0, y)),
                None,
                TextWeight::Regular,
                false,
                true,
            );
            y += 19.0;
            if y > card.y1 - 16.0 {
                break;
            }
        }
    }

    /// Draw the Add form's custom scrim/card around shared text fields. The
    /// action-confirm modal is entirely owned by [`ModalDialog`] in `draw`.
    fn draw_add_modal_overlay(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        lay: &LayoutRects,
    ) {
        if self.modal != Modal::Add {
            return;
        }
        // Scrim.
        scene.fill(
            Fill::NonZero,
            t,
            theme.tokens.colors.surfaces.overlay,
            None,
            &self.rect,
        );
        let card = lay.modal_card;
        fill_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.xl),
            theme.tokens.colors.surfaces.raised,
        );
        stroke_round(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.xl),
            ui::tokens::colors::Colors::hairline_on(theme.tokens.colors.surfaces.raised, 51),
            1.0,
        );
        self.draw_add_modal(scene, tr, t, theme, presence, card);
    }

    fn draw_add_modal(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        presence: &PresenceCtx,
        card: Rect,
    ) {
        let x = card.x0 + 28.0;
        text_w(
            tr,
            scene,
            t,
            "Import VPN profile",
            18.0,
            theme.text_primary,
            x,
            card.y0 + 24.0,
            TextWeight::Bold,
        );
        text_w(
            tr,
            scene,
            t,
            "Protocol",
            11.0,
            theme.text_dim,
            x,
            card.y0 + 60.0,
            TextWeight::Bold,
        );
        self.draw_add_kind_selector(scene, tr, t, theme, card, x);
        Self::draw_add_fields(scene, tr, t, theme, presence, card);
        self.draw_add_modal_footer(scene, tr, t, theme, card);
    }

    /// The `WireGuard`/`OpenVPN` segmented protocol selector in the add
    /// modal. Split out of [`Self::draw_add_modal`] to keep that function's
    /// line count in check.
    #[allow(
        clippy::too_many_arguments,
        reason = "the selector is scene + renderer + transform + theme + the modal card + its left edge x; grouping into a struct would only move the fields, not reduce them"
    )]
    fn draw_add_kind_selector(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        card: Rect,
        x: f64,
    ) {
        // segmented kind selector
        let seg_y = card.y0 + 78.0;
        for (i, k) in [ConnKind::WireGuard, ConnKind::OpenVpn]
            .into_iter()
            .enumerate()
        {
            let sx = x + i as f64 * 130.0;
            let r = Rect::new(sx, seg_y, sx + 122.0, seg_y + 34.0);
            let active = self.add_kind == k;
            let bg = if active {
                theme.tokens.colors.accent[5].multiply_alpha(0.22)
            } else {
                theme.tokens.colors.surfaces.base
            };
            fill_round(scene, t, r, f64::from(theme.tokens.radii.md), bg);
            stroke_round(
                scene,
                t,
                r,
                f64::from(theme.tokens.radii.md),
                if active {
                    theme.tokens.colors.accent[5]
                } else {
                    theme.tokens.colors.border_standard
                },
                1.0,
            );
            let lbl = format!("{} (.{})", k.long_name(), k.ext());
            text_center(
                tr,
                scene,
                t,
                &lbl,
                12.0,
                theme.text_primary,
                r,
                if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                },
            );
        }
    }

    /// Honest provisioning and credential-ownership explanation.
    fn draw_add_fields(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        _presence: &PresenceCtx,
        card: Rect,
    ) {
        let x = card.x0 + 28.0;
        let credential_line = if card.width() < 420.0 {
            "Credentials stay in NetworkManager."
        } else {
            "Credentials stay with NetworkManager and are never synchronized."
        };
        for (line, y, weight) in [
            (
                "Choose a complete provider configuration.",
                card.y0 + 136.0,
                TextWeight::Semibold,
            ),
            (
                "Required keys, server identity, endpoint and routes are validated",
                card.y0 + 164.0,
                TextWeight::Regular,
            ),
            (
                "before NetworkManager creates the connection.",
                card.y0 + 184.0,
                TextWeight::Regular,
            ),
            (credential_line, card.y0 + 216.0, TextWeight::Regular),
        ] {
            text_w(
                tr,
                scene,
                t,
                line,
                if weight == TextWeight::Semibold {
                    13.0
                } else {
                    11.0
                },
                if weight == TextWeight::Semibold {
                    theme.text_primary
                } else {
                    theme.text_dim
                },
                x,
                y,
                weight,
            );
        }
    }

    /// The Cancel/Save buttons and the import-tip line at the bottom of the
    /// add modal. Split out of [`Self::draw_add_modal`].
    fn draw_add_modal_footer(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        card: Rect,
    ) {
        let (cancel, save) = add_modal_button_rects(card);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(cancel, "Cancel")
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(13.0)
                .hovered(point_in(cancel, self.last_mouse)),
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(save, "Choose file")
                .primary()
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(13.0)
                .weight(TextWeight::Semibold)
                .hovered(point_in(save, self.last_mouse)),
        );
        text_w(
            tr,
            scene,
            t,
            "No profile is saved if validation or import fails.",
            11.0,
            theme.text_dim,
            card.x0 + 28.0,
            cancel.y0 - 24.0,
            TextWeight::Regular,
        );
    }

    /// Copy for the pending action's confirm dialog: title, summary, the exact
    /// command that will run, and whether the affirmative is destructive.
    fn confirm_copy(&self) -> (&'static str, String, String, bool) {
        match &self.pending {
            Some(Pending::Connect(_, p)) => {
                ("Connect tunnel", p.summary.clone(), p.display(), false)
            }
            Some(Pending::Disconnect(_, p)) => {
                ("Disconnect tunnel", p.summary.clone(), p.display(), true)
            }
            Some(Pending::TorGlobal(on, p)) => (
                if *on { "Enable Tor" } else { "Disable Tor" },
                p.summary.clone(),
                p.display(),
                false,
            ),
            None => ("Confirm", String::new(), String::new(), false),
        }
    }

    /// Build the confirm dialog for the pending action. The affirmative button
    /// is destructive for a disconnect (danger-coloured).
    fn build_confirm_dialog(&self) -> ConfirmDlg {
        let (title, summary, cmd, danger) = self.confirm_copy();
        let ok = Rc::new(Cell::new(false));
        let cancel = Rc::new(Cell::new(false));
        let dialog = ModalDialog::new(title, ConfirmBody::new(summary, cmd))
            .open(true)
            .with_max_size(480.0, 260.0)
            .with_destructive(danger)
            .primary_action("Confirm", {
                let c = ok.clone();
                move || c.set(true)
            })
            .secondary_action("Cancel", {
                let c = cancel.clone();
                move || c.set(true)
            })
            .on_dismiss({
                let c = cancel.clone();
                move || c.set(true)
            });
        ConfirmDlg {
            dialog,
            confirm: ok,
            cancel,
        }
    }

    fn ensure_confirm_dialog(&mut self) {
        if self.confirm_dialog.is_none() {
            self.confirm_dialog = Some(self.build_confirm_dialog());
        }
    }

    /// Lay the confirm dialog out against the app rect so its box + footer
    /// buttons resolve.
    fn layout_confirm_dialog(&mut self) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let rect = self.rect;
        let Some(cd) = self.confirm_dialog.as_mut() else {
            return;
        };
        let theme = Theme::dark();
        let mut text_renderer = TextRenderer::new();
        cd.dialog.layout_in(rect, &theme, &mut text_renderer);
    }

    /// Route an event into the open confirm dialog, then apply the flags its
    /// callbacks set (Enter/`y` confirm, `n` cancels — kept as shortcuts).
    fn route_confirm_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        handled: &mut bool,
    ) {
        if let WindowEvent::KeyboardInput { event: ke, .. } = event
            && ke.state == ElementState::Pressed
        {
            match &ke.logical_key {
                Key::Named(NamedKey::Enter) => {
                    self.confirm_pending(doc);
                    self.confirm_dialog = None;
                    *handled = true;
                    return;
                }
                Key::Character(c) if c.as_str() == "y" => {
                    self.confirm_pending(doc);
                    self.confirm_dialog = None;
                    *handled = true;
                    return;
                }
                Key::Character(c) if c.as_str() == "n" => {
                    self.pending = None;
                    self.modal = Modal::None;
                    self.confirm_dialog = None;
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }
        self.ensure_confirm_dialog();
        self.layout_confirm_dialog();
        let (ok, cancel) = {
            let Some(cd) = self.confirm_dialog.as_mut() else {
                return;
            };
            *handled =
                cd.dialog
                    .handle_detached(event, scale_factor, modifiers, doc, &automerge::ROOT);
            (cd.confirm.replace(false), cd.cancel.replace(false))
        };
        if ok {
            self.confirm_pending(doc);
            self.confirm_dialog = None;
        } else if cancel {
            self.pending = None;
            self.modal = Modal::None;
            self.confirm_dialog = None;
        }
        *handled = true;
    }

    fn add_modal_hit(&mut self, p: Point, lay: &LayoutRects) {
        let card = lay.modal_card;
        // kind segments
        let seg_y = card.y0 + 78.0;
        for (i, k) in [ConnKind::WireGuard, ConnKind::OpenVpn]
            .into_iter()
            .enumerate()
        {
            let sx = card.x0 + 28.0 + i as f64 * 130.0;
            let r = Rect::new(sx, seg_y, sx + 122.0, seg_y + 34.0);
            if point_in(r, p) {
                self.add_kind = k;
                return;
            }
        }
        // Draw and hit-testing share this geometry, so a themed face never
        // drifts away from its click target as the modal is resized.
        let (cancel, save) = add_modal_button_rects(card);
        if point_in(cancel, p) {
            self.modal = Modal::None;
        } else if point_in(save, p) {
            self.commit_add();
        }
    }

    /// Route modal-level Escape/Enter and pointer input while the import
    /// chooser is open. Every event is consumed so nothing reaches the
    /// connection list behind the scrim.
    fn route_add_modal_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            match logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.modal = Modal::None;
                    *handled = true;
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    self.commit_add();
                    *handled = true;
                    return;
                }
                _ => {}
            }
        }

        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            }
        ) {
            let lay = self.layout();
            self.add_modal_hit(self.last_mouse, &lay);
            if self.modal != Modal::Add {
                *handled = true;
                return;
            }
        }

        *handled = true;
    }
}

fn add_modal_button_rects(card: Rect) -> (Rect, Rect) {
    let by = card.y1 - 52.0;
    (
        Rect::new(
            card.x1 - 28.0 - 200.0,
            by,
            card.x1 - 28.0 - 110.0,
            by + 36.0,
        ),
        Rect::new(card.x1 - 28.0 - 100.0, by, card.x1 - 28.0, by + 36.0),
    )
}

fn render_config_text(c: &Connection) -> String {
    match c.kind {
        ConnKind::WireGuard => format!(
            "[Interface]\nPrivateKey = (hidden)\nAddress = {}\nDNS = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {}\nPersistentKeepalive = 25",
            c.address, c.dns, c.public_key, c.endpoint, c.allowed_ips
        ),
        ConnKind::OpenVpn => format!(
            "client\ndev tun\nproto udp\nremote {}\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\nremote-cert-tls server\n# pushed routes: {}\n# dns: {}\n<ca>\n  (certificate)\n</ca>",
            c.endpoint
                .replace(':', " ")
                .split(" (")
                .next()
                .unwrap_or(&c.endpoint),
            c.allowed_ips,
            c.dns
        ),
    }
}

/// Build the log view from REAL derived state: the true interface name, the
/// real up/down status, and the real kernel byte counters (`xfer`) when a live
/// reading exists. Byte lines are omitted rather than fabricated when no
/// counter has been read. The activity log below records real command results.
fn synth_logs(c: &Connection, activity: &[String], xfer: Option<Transfer>) -> Vec<String> {
    let mut out = Vec::new();
    let iface = config::iface_name(c);
    if c.is_connected() {
        out.push(format!("[{}] interface up — assigned {}", iface, c.address));
        if !c.allowed_ips.is_empty() {
            out.push(format!(
                "[{}] routing {} via {}",
                iface, c.allowed_ips, iface
            ));
        }
        if let Some(x) = xfer {
            out.push(format!(
                "[{}] received {} (kernel counter)",
                iface,
                ui::format::bytes(x.rx)
            ));
            out.push(format!(
                "[{}] sent {} (kernel counter)",
                iface,
                ui::format::bytes(x.tx)
            ));
        }
        if c.tor {
            out.push(format!(
                "[{iface}] traffic chained over Tor SOCKS 127.0.0.1:9050"
            ));
        }
    } else {
        out.push(format!("[{iface}] interface down"));
    }
    // append global activity newest-last
    for a in activity.iter().rev().take(8) {
        out.push(a.clone());
    }
    out
}

impl Widget for VpnApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            "vpn.toggle-tunnel",
            actions::VPN_IMPORT_TYPE,
            actions::VPN_DELETE_TYPE,
        ]
    }

    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
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
                qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
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
                qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                }
            });
        }
        if let Some((id, name, invocation)) = self.queued_delete.take() {
            self.completed_delete = Some(match os_actions::delete_vpn_connection(&name) {
                Ok(()) => {
                    let action = actions::remove_connection_action(id);
                    self.actions.dispatch_with_context(
                        state,
                        VPN_ACTION_TYPE,
                        &action,
                        qos_actions::ActionContext::for_action("org.qos.Vpn", "VPN", &action),
                    );
                    self.local_selected = None;
                    self.live_status.remove(&id);
                    self.operation_error = None;
                    qos_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: qos_actions::ActionOutput::default(),
                    }
                }
                Err(message) => {
                    self.operation_error = Some(format!("Could not delete '{name}': {message}"));
                    qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
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

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Background.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        let lay = self.layout();
        if lay.compact {
            if self.compact_detail_open {
                self.draw_detail(scene, text_renderer, transform, theme, &lay);
            } else {
                self.draw_list(scene, text_renderer, transform, theme, &lay);
            }
        } else {
            self.draw_list(scene, text_renderer, transform, theme, &lay);
            self.draw_detail(scene, text_renderer, transform, theme, &lay);
        }
        self.draw_header(scene, text_renderer, transform, theme, &lay);
        // The Add form keeps its compact protocol/card layout but embeds
        // shared text fields; action confirms use the shared ModalDialog.
        match self.modal {
            Modal::Add => {
                self.draw_add_modal_overlay(scene, text_renderer, transform, theme, presence, &lay);
            }
            Modal::Confirm => {
                self.ensure_confirm_dialog();
                self.layout_confirm_dialog();
                if let Some(cd) = self.confirm_dialog.as_mut() {
                    cd.dialog
                        .draw(scene, transform, text_renderer, theme, presence);
                }
            }
            Modal::None => self.confirm_dialog = None,
        }
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        if self.modal == Modal::Add {
            self.route_add_modal_event(event, scale_factor, handled);
            return;
        }
        // The Confirm dialog is modal: while it's up the shared ModalDialog
        // captures every event (clicks, Esc, y/n) and nothing leaks to the
        // list behind the scrim.
        if self.modal == Modal::Confirm {
            self.route_confirm_event(event, scale_factor, *modifiers, state, handled);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, scale_factor, handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_detail_scroll(delta, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(state, handled);
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => {
                self.handle_key_press(logical_key, handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "VpnApp"
    }

    /// Publish the vault + connections resource tree to the shell's Ctrl+K
    /// palette (see [`crate::actions`]).
    fn resources(&self, _doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        let mut resources = actions::provide_resources(&self.vault);
        if let Some(root) = resources.first_mut() {
            for connection in &self.vault.connections {
                let Some(resource) = root
                    .children
                    .iter_mut()
                    .find(|resource| resource.id.0 == format!("vpn.connection:{}", connection.id))
                else {
                    continue;
                };
                let connecting = connection.status == ConnStatus::Connecting;
                let (id, label) = if connection.is_connected() {
                    ("disconnect", "Disconnect")
                } else {
                    ("connect", "Connect")
                };
                if let Ok(spec) = qos_actions::ActionSpec::operation(
                    id,
                    label,
                    qos_actions::OperationKind::Effect,
                    "vpn.toggle-tunnel",
                    &connection.id.to_string(),
                ) {
                    resource.actions.insert(
                        0,
                        spec.with_contract(qos_actions::ActionContract {
                            availability: if connecting {
                                qos_actions::Availability::Unavailable {
                                    code: "transition_in_progress".into(),
                                    reason: "The tunnel is already changing state".into(),
                                }
                            } else {
                                qos_actions::Availability::Available
                            },
                            permission: qos_actions::PermissionRequirement::Admin,
                            risk: qos_actions::RiskLevel::Privileged,
                            confirmation: qos_actions::ConfirmationPolicy::Confirm,
                            retry: qos_actions::RetryPolicy::AtMostOnce,
                            execution: qos_actions::ExecutionMode::Background { cancellable: true },
                            result: qos_actions::ResultSpec::Job,
                            ..qos_actions::ActionContract::default()
                        }),
                    );
                }
            }
        }
        resources
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        _payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind != qos_actions::OperationKind::Effect
            || !matches!(
                type_id,
                "vpn.toggle-tunnel" | actions::VPN_IMPORT_TYPE | actions::VPN_DELETE_TYPE
            )
        {
            return Ok(false);
        }
        Err("VPN machine changes require the lifecycle-capable operation host".into())
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if kind != qos_actions::OperationKind::Effect || type_id != "vpn.toggle-tunnel" {
            if kind == qos_actions::OperationKind::Effect && type_id == actions::VPN_DELETE_TYPE {
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
                self.queued_delete = Some((id, name, invocation));
                return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
            }
            if kind == qos_actions::OperationKind::Effect && type_id == actions::VPN_IMPORT_TYPE {
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
                self.request_import(kind);
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

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
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
            return Some(qos_actions::InvocationUpdate::Cancelled {
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
            return Some(qos_actions::InvocationUpdate::Cancelled {
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

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its action, so it records exactly one undo boundary bracketing it — the
    /// same path a Ctrl+K palette dispatch takes.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_control_invocation.take()
    }

    fn test_id(&self) -> Option<&str> {
        Some("vpn_app")
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one semantic mirror keeps every custom-painted control aligned with the shared layout table"
    )]
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Politeness, Role};
        let lay = self.layout();
        let button = |name: &str, target: &str, bounds: Rect| {
            AccessNode::new(Role::Button)
                .with_name(name)
                .with_action_target(target)
                .with_bounds(bounds)
                .with_state(AccessState::new().focusable(true))
        };
        let mut children = vec![
            button(
                "Import WireGuard profile",
                "vpn:import-wireguard",
                lay.imp_conf,
            ),
            button("Import OpenVPN profile", "vpn:import-openvpn", lay.imp_ovpn),
            AccessNode::new(Role::Switch)
                .with_name("Route all VPN traffic over Tor")
                .with_action_target("vpn:tor-global")
                .with_bounds(lay.tor_pill)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.vault.tor_global),
                ),
            button(
                "Choose a VPN profile to import",
                "vpn:import-dialog",
                lay.add_btn,
            ),
        ];
        if !lay.compact || !self.compact_detail_open {
            children.push(
                AccessNode::new(Role::List)
                    .with_name("Saved VPN profiles")
                    .with_children(
                        lay.rows
                            .iter()
                            .filter_map(|(id, row, toggle)| {
                                let connection = self.vault.get(*id)?;
                                Some(
                                    AccessNode::new(Role::ListItem)
                                        .with_name(format!(
                                            "{}, {}, {}",
                                            connection.name,
                                            connection.kind.long_name(),
                                            connection.status.label()
                                        ))
                                        .with_children(vec![
                                            button(
                                                &format!("Show {} details", connection.name),
                                                &format!("vpn:select:{id}"),
                                                *row,
                                            ),
                                            AccessNode::new(Role::Switch)
                                                .with_name(format!("Connect {}", connection.name))
                                                .with_action_target(format!("vpn:toggle:{id}"))
                                                .with_bounds(*toggle)
                                                .with_state(
                                                    AccessState::new()
                                                        .focusable(true)
                                                        .checked(connection.is_connected()),
                                                ),
                                        ]),
                                )
                            })
                            .collect(),
                    ),
            );
        }
        if !lay.compact || self.compact_detail_open {
            if lay.compact {
                children.push(button("Back to connections", "vpn:back", lay.back_btn));
            }
            if let Some(connection) = self.vault.selected_conn() {
                children.push(button(
                    if connection.is_connected() {
                        "Disconnect"
                    } else {
                        "Connect"
                    },
                    &format!("vpn:toggle:{}", connection.id),
                    lay.connect_btn,
                ));
                children.push(
                    AccessNode::new(Role::StaticText)
                        .with_name(format!(
                            "VPN profile details. Endpoint: {}. Address: {}. DNS: {}. Allowed IPs: {}. Public key: {}.",
                            connection.endpoint,
                            connection.address,
                            connection.dns,
                            connection.allowed_ips,
                            connection.public_key
                        ))
                        .with_bounds(lay.content),
                );
            }
        }
        if let Some(error) = &self.operation_error {
            if self.retry_import_kind.is_some()
                && let Some(retry) = lay.notice_retry
            {
                children.push(button(
                    "Retry VPN profile import",
                    "vpn:retry-import",
                    retry,
                ));
            }
            if let Some(dismiss) = lay.notice_dismiss {
                children.push(button("Dismiss VPN error", "vpn:dismiss-error", dismiss));
            }
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(error)
                    .with_live(Politeness::Assertive),
            );
        } else if self.import_rx.is_some() || self.shot_loading {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name("Waiting for a VPN profile from the file picker")
                    .with_live(Politeness::Polite),
            );
        }
        let children = children
            .into_iter()
            .enumerate()
            .map(|(index, child)| child.with_key(index as u64))
            .collect();
        Some(
            AccessNode::new(Role::Window)
                .with_name("VPN connections")
                .with_state(AccessState::new().focusable(true).focused(self.is_focused))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        match target {
            "vpn:import-wireguard" => self.queue_import(ConnKind::WireGuard),
            "vpn:import-openvpn" => self.queue_import(ConnKind::OpenVpn),
            "vpn:import-dialog" => self.open_add(ConnKind::WireGuard),
            "vpn:tor-global" => self.request_tor_global(),
            "vpn:back" => self.compact_detail_open = false,
            "vpn:retry-import" => self.retry_import(),
            "vpn:dismiss-error" => self.dismiss_error(),
            _ if target.starts_with("vpn:select:") => {
                let Some(id) = target
                    .strip_prefix("vpn:select:")
                    .and_then(|raw| raw.parse().ok())
                else {
                    return false;
                };
                self.select(id);
            }
            _ if target.starts_with("vpn:toggle:") => {
                let Some(id) = target
                    .strip_prefix("vpn:toggle:")
                    .and_then(|raw| raw.parse().ok())
                else {
                    return false;
                };
                self.request_toggle(id);
            }
            _ => return false,
        }
        true
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        true
    }
    fn set_focus(&mut self, f: bool) {
        self.is_focused = f;
    }
    fn is_focused(&self) -> bool {
        self.is_focused
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the frame loop (and thus `sync`) pumping while a portal import
        // is in flight, and while a real tunnel is up so its live kernel
        // counters keep refreshing. The rates shown are measured, never
        // fabricated — with execution disabled nothing polls at all.
        self.import_rx.is_some()
            || (config::exec_allowed()
                && self
                    .vault
                    .connections
                    .iter()
                    .any(super::model::Connection::is_connected))
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &ui::permissions::PermissionContext,
    ) {
        self.draw(scene, transform, text_renderer, theme, presence);
    }
}

impl VpnApp {
    /// Track the live pointer position and which list row (if any) it hovers.
    /// Split out of [`Self::handle_event`] to keep that function's line
    /// count in check.
    fn handle_cursor_moved(
        &mut self,
        position: &winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.last_mouse = p;
        let lay = self.layout();
        let mut hov = None;
        for (id, row, _) in &lay.rows {
            if point_in(*row, p) {
                hov = Some(*id);
                break;
            }
        }
        if hov != self.hovered_row {
            self.hovered_row = hov;
            *handled = true;
        }
    }

    /// A left click: the add modal, header controls, detail tabs/connect/tor,
    /// and list rows (their toggle switch or the row itself). Split out of
    /// [`Self::handle_event`].
    fn handle_left_click(&mut self, state: &mut AutoCommit, handled: &mut bool) {
        let p = self.last_mouse;
        let lay = self.layout();
        *handled = true;
        if self.operation_error.is_some() {
            if self.retry_import_kind.is_some()
                && lay.notice_retry.is_some_and(|retry| point_in(retry, p))
            {
                self.retry_import();
                return;
            }
            if lay
                .notice_dismiss
                .is_some_and(|dismiss| point_in(dismiss, p))
            {
                self.dismiss_error();
                return;
            }
        }
        if lay.compact && self.compact_detail_open && point_in(lay.back_btn, p) {
            self.compact_detail_open = false;
            return;
        }
        // Header controls.
        if point_in(lay.add_btn, p) {
            self.open_add(ConnKind::WireGuard);
            return;
        }
        if point_in(lay.tor_pill, p) {
            self.request_tor_global();
            return;
        }
        if point_in(lay.imp_conf, p) {
            self.queue_import(ConnKind::WireGuard);
            return;
        }
        if point_in(lay.imp_ovpn, p) {
            self.queue_import(ConnKind::OpenVpn);
            return;
        }
        // Detail tabs + connect button + per-tunnel tor.
        for (tab, r) in &lay.tabs {
            if point_in(*r, p) {
                self.detail_tab = *tab;
                self.detail_scroll = 0.0;
                return;
            }
        }
        if point_in(lay.connect_btn, p) {
            if let Some(id) = self.vault.selected {
                self.request_toggle(id);
            }
            return;
        }
        let tor_row = Rect::new(
            lay.tor_row.x0,
            lay.tor_row.y0 - self.detail_scroll,
            lay.tor_row.x1,
            lay.tor_row.y1 - self.detail_scroll,
        );
        if self.detail_tab == DetailTab::Status && point_in(tor_row, p) {
            if let Some(id) = self.vault.selected {
                self.toggle_conn_tor(state, id);
            }
            return;
        }
        // List rows.
        for (id, _row, toggle) in &lay.rows {
            if point_in(*toggle, p) {
                self.request_toggle(*id);
                return;
            }
        }
        for (id, row, _) in &lay.rows {
            if point_in(*row, p) {
                self.select(*id);
                return;
            }
        }
    }

    fn handle_detail_scroll(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        let lay = self.layout();
        if !lay.compact || !self.compact_detail_open || !point_in(lay.detail, self.last_mouse) {
            return;
        }
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 32.0,
            MouseScrollDelta::PixelDelta(position) => position.y,
        };
        let content_height: f64 = match self.detail_tab {
            DetailTab::Status => 410.0,
            DetailTab::Config => 360.0,
            DetailTab::Logs => 420.0,
        };
        let max_scroll = (content_height - lay.content.height()).max(0.0);
        self.detail_scroll = (self.detail_scroll - delta_y).clamp(0.0, max_scroll);
        *handled = true;
    }

    /// A pressed global shortcut (the Add modal returns before reaching this
    /// helper). Split out of [`Self::handle_event`].
    fn handle_key_press(&mut self, logical_key: &Key, handled: &mut bool) {
        // Global shortcuts.
        match logical_key {
            Key::Named(NamedKey::Escape) if self.operation_error.is_some() => {
                self.dismiss_error();
                *handled = true;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.select_offset(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.select_offset(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Enter) => {
                if let Some(id) = self.vault.selected {
                    self.request_toggle(id);
                }
                *handled = true;
            }
            Key::Character(c) => match c.as_str() {
                "a" => {
                    self.open_add(ConnKind::WireGuard);
                    *handled = true;
                }
                "i" => {
                    self.queue_import(ConnKind::WireGuard);
                    *handled = true;
                }
                "t" => {
                    self.request_tor_global();
                    *handled = true;
                }
                "1" => {
                    self.detail_tab = DetailTab::Status;
                    *handled = true;
                }
                "2" => {
                    self.detail_tab = DetailTab::Config;
                    *handled = true;
                }
                "3" => {
                    self.detail_tab = DetailTab::Logs;
                    *handled = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The demo build populated + read back — used by the tests that need a
    /// fleet to operate on. The live app never fabricates this.
    fn demo_with_doc() -> (VpnApp, AutoCommit) {
        let mut app = VpnApp::new_demo();
        let mut doc = AutoCommit::new();
        app.refresh(&mut doc);
        (app, doc)
    }

    #[test]
    fn typed_profile_context_selects_profile_and_config_tab_after_load() {
        let mut app = VpnApp::new_demo();
        app.pending_tool_intent = Some(os_actions::ToolIntent::Vpn {
            route: os_actions::VpnRoute::Profile,
            profile: Some("Work".into()),
        });
        let mut doc = AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert_eq!(
            app.vault.selected,
            app.vault
                .connections
                .iter()
                .find(|c| c.name == "Work")
                .map(|c| c.id)
        );
        assert_eq!(app.detail_tab, DetailTab::Config);
        assert!(app.pending_tool_intent.is_none());
    }

    fn cursor_at(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        }
    }
    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }
    fn key(logical_key: Key, text: Option<&str>) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = logical_key;
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(
            winit::keyboard::NativeKeyCode::Unidentified,
        );
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = text.map(Into::into);
        WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: ev,
            is_synthetic: false,
        }
    }
    fn esc_key() -> WindowEvent {
        key(Key::Named(NamedKey::Escape), None)
    }
    fn dispatch(app: &mut VpnApp, doc: &mut AutoCommit, ev: &WindowEvent) {
        dispatch_with_modifiers(app, doc, ev, Modifiers::default());
    }
    fn dispatch_with_modifiers(
        app: &mut VpnApp,
        doc: &mut AutoCommit,
        ev: &WindowEvent,
        modifiers: Modifiers,
    ) {
        let taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy::NodeId::new(0);
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            1.0,
            &modifiers,
            doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// The action confirm routes through the shared `ModalDialog`: Escape
    /// dismisses it, and a click on its primary button runs the real
    /// `confirm_pending` — both driven through the production `handle_event`.
    #[test]
    #[serial_test::serial(vpn_env)]
    fn confirm_dialog_routes_through_modaldialog() {
        unsafe { std::env::remove_var("QOS_VPN_ALLOW_EXEC") };
        let (mut app, mut doc) = demo_with_doc();
        app.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        let work = app
            .vault
            .connections
            .iter()
            .find(|c| c.name == "Work")
            .unwrap()
            .id;
        app.select(work);

        // Arm a confirm, then Escape it — the dialog owns dismissal.
        app.request_toggle(work);
        assert_eq!(app.modal, Modal::Confirm);
        dispatch(&mut app, &mut doc, &esc_key());
        assert_eq!(app.modal, Modal::None, "Escape closed the confirm dialog");
        assert!(app.pending.is_none(), "Escape cleared the pending action");
        assert!(app.confirm_dialog.is_none());

        // Re-arm, then click the dialog's primary → confirm_pending runs.
        app.request_toggle(work);
        assert_eq!(app.modal, Modal::Confirm);
        dispatch(&mut app, &mut doc, &cursor_at(Point::new(10.0, 10.0)));
        let primary = app.confirm_dialog.as_ref().unwrap().dialog.primary_rect();
        dispatch(&mut app, &mut doc, &cursor_at(primary.center()));
        dispatch(&mut app, &mut doc, &press());
        assert_eq!(
            app.modal,
            Modal::None,
            "the primary click resolved the confirm"
        );
        assert!(
            app.pending.is_none(),
            "confirming consumed the pending action"
        );
        assert!(app.confirm_dialog.is_none());
    }

    #[test]
    fn live_new_starts_empty_and_writes_nothing() {
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

    #[test]
    fn demo_seeds_fleet_for_shots() {
        // `--shots` still needs a populated frame.
        let (app, _doc) = demo_with_doc();
        assert_eq!(app.vault.connections.len(), 4);
        assert!(app.vault.selected.is_some());
    }

    #[test]
    fn select_offset_wraps() {
        let (mut app, _doc) = demo_with_doc();
        let first = app.vault.selected.unwrap();
        app.select_offset(-1);
        let last = app.vault.selected.unwrap();
        assert_ne!(first, last);
        app.select_offset(1);
        assert_eq!(app.vault.selected.unwrap(), first);
    }

    #[test]
    fn connecting_profile_rejects_a_duplicate_transition() {
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

    #[test]
    fn error_notice_can_be_dismissed_with_escape() {
        let (mut app, mut doc) = demo_with_doc();
        app.operation_error = Some("The profile is incomplete".into());
        app.retry_import_kind = Some(ConnKind::WireGuard);

        dispatch(&mut app, &mut doc, &esc_key());

        assert!(app.operation_error.is_none());
        assert!(app.retry_import_kind.is_none());
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn confirm_connect_without_exec_stays_disconnected() {
        // Honesty: with execution disabled (the default) confirming a connect
        // must NOT claim the tunnel is connected — the real command never ran.
        unsafe { std::env::remove_var("QOS_VPN_ALLOW_EXEC") };
        let (mut app, mut doc) = demo_with_doc();
        let work = app
            .vault
            .connections
            .iter()
            .find(|c| c.name == "Work")
            .unwrap()
            .id;
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

    #[test]
    #[serial_test::serial(vpn_env)]
    fn import_parses_metadata_without_duplicating_the_secret_file() {
        // NetworkManager owns the imported profile and secrets. In a test
        // process host import is disabled, but the editor artifact is parsed
        // without retaining a second raw config/private-key file.
        let (mut app, mut doc) = demo_with_doc();
        let before = app.vault.connections.len();
        let raw = "[Interface]\nPrivateKey = c2VjcmV0\nAddress = 10.9.0.2/32\nDNS = 9.9.9.9\n\n[Peer]\nPublicKey = abc=\nEndpoint = imported.example:51820\nAllowedIPs = 0.0.0.0/0\n";
        let path = std::env::temp_dir().join(format!("home-{}.conf", uuid::Uuid::new_v4()));
        std::fs::write(&path, raw).unwrap();

        app.import_rx = Some(ui::portal::inject_for_test(PortalOutcome::Opened(
            path.clone(),
        )));
        assert!(app.drain_import(&mut doc), "a delivered import applies");
        assert!(app.import_rx.is_none(), "draining clears the request");
        assert_eq!(app.vault.connections.len(), before + 1);

        let imported = app
            .vault
            .connections
            .iter()
            .find(|c| c.endpoint == "imported.example:51820")
            .expect("imported tunnel present with parsed endpoint")
            .clone();
        assert_eq!(imported.kind, ConnKind::WireGuard);
        assert_eq!(imported.dns, "9.9.9.9");

        assert_eq!(imported.config_path(), None, "NetworkManager owns secrets");

        // The live plan uses the shared NetworkManager backend.
        let plan = config::connect_plan(&imported, config::default_backend(imported.kind));
        assert_eq!(plan.program, "nmcli");
        assert_eq!(plan.args, vec!["connection", "up", imported.name.as_str()]);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn add_modal_never_commits_an_unprovisioned_connection() {
        let (mut app, _doc) = demo_with_doc();
        let before = app.vault.connections.len();
        app.open_add(ConnKind::OpenVpn);
        app.commit_add();
        assert_eq!(app.vault.connections.len(), before);
        assert!(app.pending_control_invocation.is_some());
    }

    #[test]
    fn add_modal_shared_button_rects_drive_save_and_cancel() {
        let (mut app, mut doc) = demo_with_doc();
        app.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
        let before = app.vault.connections.len();
        app.open_add(ConnKind::WireGuard);
        let card = app.layout().modal_card;
        let (cancel, save) = add_modal_button_rects(card);
        assert!(cancel.x1 <= save.x0, "button rects do not overlap");

        dispatch(&mut app, &mut doc, &cursor_at(save.center()));
        dispatch(&mut app, &mut doc, &press());
        assert_eq!(app.vault.connections.len(), before);
        assert!(app.pending_control_invocation.is_some());
        assert_eq!(app.modal, Modal::None);

        app.open_add(ConnKind::OpenVpn);
        let card = app.layout().modal_card;
        let (cancel, _) = add_modal_button_rects(card);
        dispatch(&mut app, &mut doc, &cursor_at(cancel.center()));
        dispatch(&mut app, &mut doc, &press());
        assert_eq!(app.vault.connections.len(), before);
        assert_eq!(app.modal, Modal::None, "Cancel dismisses without saving");
    }

    #[test]
    fn add_modal_does_not_capture_credentials_and_enter_queues_import() {
        let (mut app, mut doc) = demo_with_doc();
        let before = app.vault.connections.len();
        app.open_add(ConnKind::WireGuard);

        dispatch(
            &mut app,
            &mut doc,
            &WindowEvent::Ime(winit::event::Ime::Commit("secret".into())),
        );
        dispatch(&mut app, &mut doc, &key(Key::Named(NamedKey::Enter), None));
        assert_eq!(app.modal, Modal::None);
        assert_eq!(app.vault.connections.len(), before);
        assert!(app.pending_control_invocation.is_some());
    }

    #[test]
    fn exported_tunnel_toggle_is_a_cancellable_background_job() {
        let (mut app, doc) = demo_with_doc();
        let connection = app.vault.connections[0].id;
        let invocation = qos_actions::InvocationId::new();
        let started = app
            .start_resource_operation(
                invocation,
                qos_actions::OperationKind::Effect,
                "vpn.toggle-tunnel",
                &serde_json::to_vec(&connection.to_string()).unwrap(),
                &doc,
                &automerge::ROOT,
            )
            .unwrap();
        assert_eq!(started, Some(ui::widget::ResourceOperationStart::Accepted));
        assert!(matches!(
            app.poll_resource_operation(invocation, true, &doc, &automerge::ROOT),
            Some(qos_actions::InvocationUpdate::Cancelled { .. })
        ));

        let action = Widget::resources(&app, &doc)
            .into_iter()
            .flat_map(|root| root.children)
            .flat_map(|connection| connection.actions)
            .find(|action| action.id == "connect" || action.id == "disconnect")
            .unwrap();
        assert!(matches!(
            action.contract.execution,
            qos_actions::ExecutionMode::Background { cancellable: true }
        ));
        assert_eq!(action.contract.result, qos_actions::ResultSpec::Job);
    }
}
