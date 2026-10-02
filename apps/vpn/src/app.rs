//! Root widget for the VPN client.
//!
//! `VpnApp` is a single full-canvas [`Widget`] that owns the data flow:
//! it seeds + reads the [`Vault`] from the shared Automerge document,
//! lays out a connections list + detail pane + footer itself (no taffy
//! sub-tree), draws everything into the Vello scene, and routes its own
//! pointer/keyboard events back into CRDT mutations. Throughput is an
//! ephemeral, time-driven animation layered on top.

use std::collections::{HashMap, HashSet};
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use std::cell::Cell;
use std::rc::Rc;
use ui::charts::{Chart, ChartLabels, ChartStyle, Series, TrendRange, TrendStyle};
use ui::chrome::{
    PANEL_BORDER_ALPHA, SelectionState, chrome_hairline, panel_border, panel_ground, selection_tier,
};
use ui::control_custody::RawControlSlot;
use ui::icons::Icon;
use ui::paint::{PaintCtx, fill_round, stroke_round};
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::tilo_actions::{ActionAddress, ControlInvocation};
use ui::tokens::Spacing;
use ui::widget::{
    Button, ButtonFace, EmptyState, EmptyStateContent, ModalDialog, SegmentedFace, Toggle,
    ToggleFace, ToggleGroup, Widget,
};
use zeroize::Zeroize;

use crate::actions::{self, VPN_ACTION_TYPE};
use crate::config::{self, CommandPlan};
use crate::doc::VpnDoc;
use crate::model::{
    ConnId, ConnKind, ConnStatus, Connection, LocalProfileDetails, Vault, human_ago, sample_vault,
};
use crate::net::{self, Transfer};

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

    fn has_rate(&self) -> bool {
        self.samples.len() >= 2
    }
}

const HEADER_H: f64 = 72.0;
const LIST_W: f64 = 440.0;
/// Height of the header's control band (Tor pill + Import) at 1× text. The
/// live value is measured from the button label each frame so a doubled
/// accessibility scale grows the band instead of sinking the labels.
const HEADER_CONTROL_H: f64 = 36.0;

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
/// Connection-row height: 1× floor of the renderer-measured `row_h`.
const ROW_H: f64 = 74.0;
/// The 15px name line at 1× measures 18.75px under a 14px top padding; its
/// centre, and the top of the endpoint line beneath it (44px: the name line
/// plus an 11.25px gap — the floor of the renderer-measured `row_line2_top`).
const ROW_NAME_LINE_1X: f64 = 18.75;
const ROW_NAME_CY: f64 = 14.0 + ROW_NAME_LINE_1X / 2.0;
const ROW_LINE2_TOP: f64 = 14.0 + ROW_NAME_LINE_1X + 11.25;

// ---- spacing rhythm (HIG §7b) ------------------------------------------
//
// One rhythm for the whole window, read from the unscaled token scale so a
// gap in this file is always a named step: `S2` between related rows (the
// connection list), `S3` between sibling tiles and controls, `S4` between
// sections (tiles → chart → facts), `S6` before the next group.
const S2: f64 = Spacing::BASE.s2 as f64;
const S3: f64 = Spacing::BASE.s3 as f64;
const S4: f64 = Spacing::BASE.s4 as f64;
/// Connection rows are related rows: `S2` apart.
const ROW_GAP: f64 = S2;

/// Deterministic fictional values used only by the hermetic visual-review
/// build. Live instances populate this map solely from the locally selected
/// provider file, never from collaboration.
fn demo_local_details(vault: &Vault) -> HashMap<ConnId, LocalProfileDetails> {
    vault
        .connections
        .iter()
        .map(|connection| {
            let details = match connection.kind {
                ConnKind::WireGuard => LocalProfileDetails {
                    endpoint: "demo-gateway.example:51820".into(),
                    address: "10.77.0.2/32".into(),
                    dns: "10.77.0.1".into(),
                    public_key: "demo public key ••••7Jq".into(),
                    allowed_ips: "0.0.0.0/0, ::/0".into(),
                    ..LocalProfileDetails::default()
                },
                ConnKind::OpenVpn => LocalProfileDetails {
                    endpoint: "demo-vpn.example:1194 (UDP)".into(),
                    address: "172.20.77.2/24".into(),
                    dns: "172.20.77.1".into(),
                    allowed_ips: "172.20.0.0/16".into(),
                    ..LocalProfileDetails::default()
                },
            };
            (connection.id, details)
        })
        .collect()
}

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
        PaintCtx::new(scene, t, tr, theme).text(
            &self.summary,
            &TextStyle::new(13.0),
            theme.text_dim,
            Point::new(x, self.rect.y0),
            None,
        );
        let _ = width;
        // Command preview box below the summary.
        let box_top = self.rect.y0 + 36.0;
        let cmd_box = Rect::new(x, box_top, self.rect.x1, (box_top + 56.0).min(self.rect.y1));
        // A well in the dialog: the window tier with its derived edge.
        ui::surface::card(
            scene,
            t,
            cmd_box,
            f64::from(theme.tokens.radii.md),
            theme.tokens.colors.surfaces.base,
        );
        PaintCtx::new(scene, t, tr, theme).text(
            "Will run:",
            &TextStyle::new(10.0).with_weight(TextWeight::Bold),
            theme.text_dim,
            Point::new(cmd_box.x0 + 12.0, cmd_box.y0 + 8.0),
            None,
        );
        tr.draw_style(
            scene,
            &ellipsize(&self.cmd, 52),
            theme.tokens.colors.semantic.success,
            t * Affine::translate((cmd_box.x0 + 12.0, cmd_box.y0 + 26.0)),
            None,
            &TextStyle::new(13.0).with_family(FontChoice::Monospace),
        );
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
    compact_detail_open: bool,
    /// Per-window compact-detail scroll. This is presentation state and is
    /// never synchronized with the shared profile document.
    detail_scroll: f64,
    shot_loading: bool,
    /// Measured header control height (see [`HEADER_CONTROL_H`]); refreshed
    /// from the renderer in `adjust_layout_with_measure` before geometry is
    /// applied, so layout, paint and hit-testing read one value.
    control_h: f64,
    /// Connection-row height and the top of its second line, measured from
    /// the renderer in `adjust_layout_with_measure` (`ROW_H` / 44px floors at
    /// 1×) so `layout` — which has no renderer — builds the rows the painter
    /// fills.
    row_h: f64,
    row_line2_top: f64,
    /// Centre of the name line (status dot, badge and tor mark ride it).
    row_name_cy: f64,
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
            last_mouse: Point::ZERO,
            hovered_row: None,
            is_focused: false,
            throughput: HashMap::new(),
            live_status,
            local_credentials,
            local_details,
            last_reconcile: None,
            actions: crate::resource::ActionSink::default(),
            pending_tool_intent: None,
            demo_mode,
            compact_detail_open: false,
            detail_scroll: 0.0,
            shot_loading: false,
            control_h: HEADER_CONTROL_H,
            row_h: ROW_H,
            row_line2_top: ROW_LINE2_TOP,
            row_name_cy: ROW_NAME_CY,
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
                self.local_credentials.clear();
                self.local_details.clear();
            }
            "unprovisioned" => {
                if let Some(id) = self.vault.selected {
                    self.local_credentials.remove(&id);
                    self.local_details.remove(&id);
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    self.compact_detail_open = true;
                }
            }
            "setup" => {
                if let Some(id) = self.vault.selected {
                    self.local_credentials.remove(&id);
                    self.local_details.remove(&id);
                    self.live_status.insert(id, ConnStatus::Disconnected);
                    self.provision_target = Some(id);
                    self.open_add(ConnKind::WireGuard);
                }
            }
            _ => {}
        }
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
        self.log(format!("choose a .{} file to import…", kind.ext()));
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
                                "parsed {} '{}' (host import disabled in test process)",
                                kind.badge(),
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
                                    "imported {} '{}' into NetworkManager",
                                    kind.badge(),
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
            "waiting for approval to import a complete {} provider profile",
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

    // ---- layout ------------------------------------------------------

    #[allow(
        clippy::too_many_lines,
        reason = "one geometry table is shared by paint, hit testing, and accessibility"
    )]
    fn layout(&self) -> LayoutRects {
        let r = self.rect;
        let compact = r.width() < 720.0;
        // The compact title occupies the first row; controls occupy the second.
        // The body and optional notice follow the actual control band.
        let controls_top = if compact { 52.0 } else { 18.0 };
        let controls_bottom = controls_top + self.control_h;
        // Both header forms end below the control band: the wide header keeps
        // its 72px minimum but grows once the measured band no longer fits.
        let base_header_h = if compact {
            controls_bottom + 16.0
        } else {
            HEADER_H.max(controls_bottom + controls_top)
        };
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
        let empty = self.vault.connections.is_empty();
        let list = if compact || empty {
            body
        } else {
            Rect::new(body.x0, body.y0, body.x0 + LIST_W, body.y1)
        };
        let detail = if compact || empty {
            body
        } else {
            Rect::new(list.x1, body.y0, body.x1, body.y1)
        };

        // One clear import entry point. Protocol-specific imports remain
        // available as typed resource actions for keyboard/agent invocation.
        let by0 = header.y0 + controls_top;
        let by1 = header.y0 + controls_bottom;
        let (add_btn, tor_pill, imp_conf, imp_ovpn) = if compact {
            let left = header.x0 + 16.0;
            let right = header.x1 - 16.0;
            let gap = 8.0;
            let half = (right - left - gap) / 2.0;
            (
                Rect::new(left + half + gap, by0, right, by1),
                Rect::new(left, by0, left + half, by1),
                Rect::ZERO,
                Rect::ZERO,
            )
        } else {
            let add = Rect::new(header.x1 - 24.0 - 132.0, by0, header.x1 - 24.0, by1);
            let tor = Rect::new(add.x0 - 12.0 - 150.0, by0, add.x0 - 12.0, by1);
            (add, tor, Rect::ZERO, Rect::ZERO)
        };

        // Rows.
        let mut rows = Vec::new();
        let mut y = list.y0 + 44.0; // leave room for the list section title
        for c in &self.vault.connections {
            let row = Rect::new(list.x0 + 12.0, y, list.x1 - 12.0, y + self.row_h);
            let toggle = Rect::new(
                row.x1 - 16.0 - 46.0,
                row.center().y - 13.0,
                row.x1 - 16.0,
                row.center().y + 13.0,
            );
            rows.push((c.id, row, toggle));
            y += self.row_h + ROW_GAP;
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
        // The title row cannot carry the 150px action beside the 24px name,
        // its status pill and the subtitle once the pane is narrower than
        // ~560px (a docked list beside a tablet-width window): the action
        // then takes its own full-width row under the subtitle, as it does
        // in the compact route, instead of painting over the title.
        let stack_header = compact || dinner.width() < 560.0;
        let connect_btn = if stack_header {
            Rect::new(dinner.x0, dinner.y0 + 62.0, dinner.x1, dinner.y0 + 104.0)
        } else {
            Rect::new(
                dinner.x1 - 150.0,
                dinner.y0 + 6.0,
                dinner.x1,
                dinner.y0 + 48.0,
            )
        };
        let tabs_y = dinner.y0 + if stack_header { 120.0 } else { 76.0 };
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

/// The Status tab's kv table: `(top, row stride)`. The table starts `S4`
/// under the chart `status_geometry` places, and the one stride is shared by
/// the painter, the Tor-toggle hit rect and the scroll extent so all three
/// agree (a fixed 72px tile band once put the toggle 30px low).
fn status_kv_table(content: Rect) -> (f64, f64) {
    let (_, chart) = status_geometry(content);
    (chart.y1 + S4, 30.0)
}

/// How tall the Status tab's content is: tiles, chart and the five kv rows
/// (four facts and the Tor row). The detail pane scrolls by the difference
/// to its height, so the Tor row stays reachable in a short window.
fn status_content_height(content: Rect) -> f64 {
    let (kv_top, row_h) = status_kv_table(content);
    kv_top + 5.0 * row_h - content.y0
}

/// Position of the per-tunnel-Tor pill inside the Status kv table.
fn kv_tor_rect(content: Rect) -> Rect {
    // 5th kv row.
    let (kv_top, row_h) = status_kv_table(content);
    let row_y = kv_top + 4.0 * row_h;
    Rect::new(
        content.x1 - 84.0,
        row_y + 2.0,
        content.x1 - 84.0 + 64.0,
        row_y + 26.0,
    )
}

/// The Status tab's vertical rhythm from the content rect alone, shared by
/// the painter and the hit layout: the stat-tile band height (one row of
/// 72px tiles, or two rows of 56px tiles `S3` apart when the pane is
/// narrow) and the throughput chart `S4` under it.
fn status_geometry(content: Rect) -> (f64, Rect) {
    let compact = content.width() < 500.0;
    let tiles_h = if compact { 2.0 * 56.0 + S3 } else { 72.0 };
    let chart_top = content.y0 + tiles_h + S4;
    let chart_h = if compact { 96.0 } else { 176.0 };
    (
        tiles_h,
        Rect::new(content.x0, chart_top, content.x1, chart_top + chart_h),
    )
}

fn point_in(r: Rect, p: Point) -> bool {
    p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
}

// ----------------------------------------------------------------------
// Drawing primitives
// ----------------------------------------------------------------------

/// Center text vertically within `band` and left-align at `x`.
fn text_vc(ctx: &mut PaintCtx<'_>, s: &str, style: &TextStyle, color: Color, x: f64, band: Rect) {
    // Centre on the line the renderer will paint (weight and accessibility
    // scale included), not on the nominal size: the nominal box under-reads
    // by a few pixels at 1× and by half a line at 2×.
    let (_, th) = ctx.text.measure_style(s, None, style);
    let top = band.y0 + (band.height() - f64::from(th)) / 2.0;
    ctx.text(s, style, color, Point::new(x, top), None);
}

/// Center text horizontally + vertically within a rect (button labels).
fn text_center(ctx: &mut PaintCtx<'_>, s: &str, style: &TextStyle, color: Color, r: Rect) {
    let (tw, th) = ctx.text.measure_style(s, None, style);
    let x = r.center().x - f64::from(tw) / 2.0;
    let top = r.y0 + (r.height() - f64::from(th)) / 2.0;
    ctx.text(s, style, color, Point::new(x, top), None);
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

/// One tunnel's measured transfer rates for the status tab: the per-sample
/// down/up series, whether they are a live reading, and the latest totals.
struct RateReadings {
    down: Vec<f64>,
    up: Vec<f64>,
    live: bool,
    latest: Option<Transfer>,
}

impl VpnApp {
    fn status_color(theme: &Theme, status: ConnStatus) -> Color {
        match status {
            ConnStatus::Connected => theme.tokens.colors.semantic.success,
            ConnStatus::Connecting => theme.tokens.colors.semantic.warning,
            ConnStatus::Disconnected => theme.text_dim,
        }
    }

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
        // A tinted capsule: the tint is its contrast step against the row.
        ui::surface::pill(scene, t, r, bg, None, Some(9.0));
        text_center(
            &mut PaintCtx::new(scene, t, tr, theme),
            label,
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            fg,
            r,
        );
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
        // Bar role: one derived hairline under the raised band.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            chrome_hairline(theme),
            None,
            &Line::new(Point::new(h.x0, h.y1 - 0.5), Point::new(h.x1, h.y1 - 0.5)),
        );
        // Primary left label: the contextual saved/active tally. The compositor
        // owns window chrome, so the app draws no wordmark or brand mark here.
        let sub = format!(
            "{} saved · {} active",
            self.vault.connections.len(),
            self.vault.active_count()
        );
        // The tally shares the controls' centre line when they sit beside it,
        // and centres in its own first row when the compact header stacks.
        let tally_band = if lay.compact {
            Rect::new(h.x0, h.y0, h.x1, lay.add_btn.y0)
        } else {
            lay.add_btn
        };
        text_vc(
            &mut PaintCtx::new(scene, t, tr, theme),
            &sub,
            &TextStyle::new(14.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            h.x0 + 24.0,
            tally_band,
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
            let message_w = (text_right - notice.x0 - 24.0).max(40.0) as f32;
            let message = tr.elide_to_width_weighted(&label, 12.0, message_w, TextWeight::Semibold);
            text_vc(
                &mut PaintCtx::new(scene, t, tr, theme),
                &message,
                &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
                color,
                notice.x0 + 24.0,
                notice,
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
        // Off: the window tier with its derived rest edge; on: the accent tint
        // with the accent as its edge (live state).
        ui::surface::pill(
            scene,
            t,
            r,
            bg,
            Some(if on {
                theme.tokens.colors.accent[5]
            } else {
                ui::surface::edge_on(bg)
            }),
            Some(f64::from(theme.tokens.radii.full)),
        );
        // status dot
        let dot: Color = if on {
            theme.tokens.colors.accent[6]
        } else {
            theme.text_dim
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
            &mut PaintCtx::new(scene, t, tr, theme),
            &txt,
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            r.x0 + 32.0,
            r,
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
        // The pane's right edge is derived against the tier it is painted on.
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::surface::edge_on(theme.tokens.colors.surfaces.base),
            None,
            &Line::new(Point::new(l.x1 - 0.5, l.y0), Point::new(l.x1 - 0.5, l.y1)),
        );
        PaintCtx::new(scene, t, tr, theme).text(
            "CONNECTIONS",
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            theme.text_dim,
            Point::new(l.x0 + 16.0, l.y0 + 18.0),
            None,
        );

        for (id, row, toggle) in &lay.rows {
            let Some(c) = self.vault.get(*id) else {
                continue;
            };
            self.draw_list_row(&mut PaintCtx::new(scene, t, tr, theme), c, *row, *toggle);
        }
    }

    /// One connection row: selection/hover background, status dot, name +
    /// protocol badge, endpoint/last-used line, the per-tunnel Tor flag, and
    /// the connect toggle switch. Split out of [`Self::draw_list`] to keep
    /// that function's line count in check.
    fn draw_list_row(&self, ctx: &mut PaintCtx<'_>, c: &Connection, row: Rect, toggle: Rect) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let selected = self.vault.selected == Some(c.id);
        let hovered = self.hovered_row == Some(c.id);
        let details = self.local_details.get(&c.id);
        let (name_cy_offset, line2_top) = (self.row_name_cy, self.row_line2_top);
        let radius = f64::from(theme.tokens.radii.lg);
        // Selection is a lift with a derived edge, hover half a lift (HIG
        // §9.3); the accent stays for the *live* connected state. The rows
        // sit on the window tier, so the shared tier registers on every
        // preset; the ring is the panel-strength derived edge.
        if selected {
            let tier = selection_tier(theme, SelectionState::active(true));
            fill_round(scene, t, row, radius, tier.fill);
            stroke_round(
                scene,
                t,
                row.inset(-0.5),
                radius - 0.5,
                ui::surface::edge_on_with(tier.fill, PANEL_BORDER_ALPHA),
                1.0,
            );
        } else if hovered {
            fill_round(scene, t, row, radius, theme.tokens.colors.lift(0.5));
        }

        // The first line's centre is the name's measured centre (from the
        // layout metrics); the status dot and the badges ride it.
        let name_cy = row.y0 + name_cy_offset;

        // status dot
        let sc = Self::status_color(theme, c.status);
        scene.fill(
            Fill::NonZero,
            t,
            sc,
            None,
            &Circle::new(Point::new(row.x0 + 22.0, name_cy), 5.0),
        );
        if c.is_connected() {
            scene.stroke(
                &Stroke::new(1.5),
                t,
                sc.multiply_alpha(0.4),
                None,
                &Circle::new(Point::new(row.x0 + 22.0, name_cy), 9.0),
            );
        }

        // name + badge (+ tor mark) — elided against the space the measured
        // badge, the tor mark and the toggle leave, at the weight the name is
        // painted with.
        let name_x = row.x0 + 38.0;
        let (badge_label_w, _) = tr.measure_weighted(c.kind.badge(), 11.0, None, TextWeight::Bold);
        let badge_slot = 10.0 + f64::from(badge_label_w) + 16.0;
        let tor_slot = if c.tor {
            let (tor_w, _) = tr.measure_weighted("tor", 10.0, None, TextWeight::Bold);
            f64::from(tor_w) + 8.0
        } else {
            0.0
        };
        let name_max_w = ((toggle.x0 - 16.0 - badge_slot - tor_slot) - name_x).max(40.0) as f32;
        let name = tr.elide_to_width_weighted(&c.name, 15.0, name_max_w, TextWeight::Semibold);
        PaintCtx::new(scene, t, tr, theme).text(
            &name,
            &TextStyle::new(15.0).with_weight(TextWeight::Semibold),
            theme.text_primary,
            Point::new(row.x0 + 38.0, row.y0 + 14.0),
            None,
        );
        let (nw, _) = tr.measure_weighted(&name, 15.0, None, TextWeight::Semibold);
        let badge_x = row.x0 + 38.0 + f64::from(nw) + 10.0;
        let badge_w = Self::draw_badge(scene, tr, t, theme, badge_x, name_cy, c.kind);

        // Per-tunnel Tor mark: beside the protocol badge, on the badge's own
        // centre line — it qualifies the tunnel the way the badge does, rather
        // than floating at the row's top-right above the toggle.
        if c.tor {
            let (_, tor_h) = tr.measure_weighted("tor", 10.0, None, TextWeight::Bold);
            PaintCtx::new(scene, t, tr, theme).text(
                "tor",
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                theme.tokens.colors.accent[6],
                Point::new(badge_x + badge_w + 8.0, name_cy - f64::from(tor_h) / 2.0),
                None,
            );
        }

        let endpoint = details.map_or("Credentials unavailable on this device", |d| {
            d.endpoint.as_str()
        });
        let line2_max_w = ((toggle.x0 - 16.0) - name_x).max(40.0) as f32;
        let line2 = tr.elide_to_width(&format!("{endpoint} · {}", c.last_used), 12.0, line2_max_w);
        PaintCtx::new(scene, t, tr, theme).text(
            &line2,
            &TextStyle::new(12.0),
            theme.text_dim,
            Point::new(row.x0 + 38.0, row.y0 + line2_top),
            None,
        );

        // toggle switch
        Self::draw_switch(scene, t, theme, toggle, c.is_connected());
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

        if lay.compact && !self.vault.connections.is_empty() {
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

        Self::draw_detail_header(
            scene,
            tr,
            t,
            theme,
            dinner,
            c,
            self.local_details.get(&c.id),
        );
        self.draw_detail_connect_button(scene, tr, t, theme, lay, c);
        self.draw_detail_tabs(scene, tr, t, theme, lay);

        // The tab content is clipped to its pane and scrolls whenever it is
        // taller than the pane (`detail_scroll_max`), docked or compact.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &lay.content);
        let content_transform = t * Affine::translate((0.0, -self.detail_scroll));
        match self.detail_tab {
            DetailTab::Status => {
                self.draw_status_tab(scene, tr, content_transform, theme, lay, c);
            }
            DetailTab::Config => {
                self.draw_config_tab(scene, tr, content_transform, theme, lay, c);
            }
            DetailTab::Logs => {
                self.draw_logs_tab(scene, tr, content_transform, theme, lay, c);
            }
        }
        scene.pop_layer();
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
            &mut PaintCtx::new(scene, t, tr, theme),
            d,
            &EmptyStateContent {
                icon: Some(Icon::Shield),
                title,
                message: Some(message),
                action_label: None,
            },
            Point::ZERO,
        );
    }

    /// The title, status pill and protocol subtitle at the top of the detail
    /// pane. Split out of [`Self::draw_detail`].
    fn draw_detail_header(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        dinner: Rect,
        c: &Connection,
        details: Option<&LocalProfileDetails>,
    ) {
        PaintCtx::new(scene, t, tr, theme).text(
            &c.name,
            &TextStyle::new(24.0).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(dinner.x0, dinner.y0),
            None,
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
        // A tone-tinted capsule with the tone as its edge, so the status pill
        // reads on both palettes.
        ui::surface::pill(
            scene,
            t,
            pill,
            sc.multiply_alpha(0.18),
            Some(sc.multiply_alpha(0.55)),
            Some(f64::from(theme.tokens.radii.full)),
        );
        scene.fill(
            Fill::NonZero,
            t,
            sc,
            None,
            &Circle::new(Point::new(pill.x0 + 12.0, pill.center().y), 4.0),
        );
        text_vc(
            &mut PaintCtx::new(scene, t, tr, theme),
            plabel,
            &TextStyle::new(12.0).with_weight(TextWeight::Semibold),
            sc,
            pill.x0 + 22.0,
            pill,
        );

        let sub = format!(
            "{} · {}",
            c.kind.long_name(),
            details.map_or("Credentials unavailable on this device", |d| d
                .endpoint
                .as_str())
        );
        PaintCtx::new(scene, t, tr, theme).text(
            &sub,
            &TextStyle::new(13.0),
            theme.text_dim,
            Point::new(dinner.x0, dinner.y0 + 34.0),
            None,
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
        let locally_provisioned = self.local_credentials.contains(&c.id)
            && self
                .local_details
                .get(&c.id)
                .is_some_and(|details| !details.network_manager_uuid.is_empty());
        let face = if connecting {
            ButtonFace::new(lay.connect_btn, "Connecting…").disabled(true)
        } else if !locally_provisioned {
            ButtonFace::new(lay.connect_btn, "Set up on this device").primary()
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
                ui::surface::card(
                    scene,
                    t,
                    *r,
                    f64::from(theme.tokens.radii.md),
                    theme.tokens.colors.surfaces.raised,
                );
            }
            let col = if active {
                theme.text_primary
            } else {
                theme.text_dim
            };
            text_center(
                &mut PaintCtx::new(scene, t, tr, theme),
                tab.label(),
                &TextStyle::new(13.0).with_weight(if active {
                    TextWeight::Semibold
                } else {
                    TextWeight::Regular
                }),
                col,
                *r,
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
        let details = self.local_details.get(&c.id);

        // Throughput from the kernel (`wg show transfer` / sysfs) for this
        // tunnel. `None`/empty until at least two samples exist.
        let hist = self.throughput.get(&c.id);
        let (down, up) = hist.map(TransferHistory::rate_series).unwrap_or_default();
        let rates = RateReadings {
            down,
            up,
            live: hist.is_some_and(TransferHistory::has_rate) && c.is_connected(),
            latest: hist.and_then(TransferHistory::latest),
        };
        // One geometry for the painter and the hit layout (`kv_tor_rect`).
        let (tiles_h, chart) = status_geometry(content);

        let ctx = &mut PaintCtx::new(scene, t, tr, theme);
        Self::draw_status_tiles(ctx, content, c, details, &rates, tiles_h);
        Self::draw_status_throughput(ctx, chart, &rates, c);
        Self::draw_status_kv_rows(scene, tr, t, theme, content, lay, c, details);
    }

    /// The four-across stat tiles (local state / handshake age / measured
    /// down / measured up). Split out of [`Self::draw_status_tab`] to keep
    /// that function's line count in check.
    #[allow(
        clippy::many_single_char_names,
        clippy::similar_names,
        reason = "compact geometry uses conventional x/y/rect names beside the total tile height"
    )]
    fn draw_status_tiles(
        ctx: &mut PaintCtx<'_>,
        content: Rect,
        c: &Connection,
        details: Option<&LocalProfileDetails>,
        rates: &RateReadings,
        tiles_h: f64,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let has_rate = rates.live;
        // These values are local-only: they never enter the document, actions
        // or resource search index.
        let compact = content.width() < 500.0;
        // Sibling tiles sit `S3` apart in either arrangement.
        let gap = S3;
        let columns = if compact { 2.0 } else { 4.0 };
        let tw = (content.width() - gap * (columns - 1.0)) / columns;
        let rate_or_dash = |v: Option<f64>| match v {
            Some(bps) if has_rate => ui::format::rate(bps),
            _ => "—".to_string(),
        };
        let tile_vals = [
            (
                "ASSIGNED IP",
                details.map_or_else(|| "—".into(), |d| d.address.clone()),
            ),
            (
                "HANDSHAKE",
                match c.last_handshake_secs {
                    Some(s) if c.is_connected() => human_ago(s),
                    _ => "—".to_string(),
                },
            ),
            ("DOWN", rate_or_dash(rates.down.last().copied())),
            ("UP", rate_or_dash(rates.up.last().copied())),
        ];
        for (i, (label, val)) in tile_vals.iter().enumerate() {
            let column = if compact { i % 2 } else { i };
            let row = if compact { i / 2 } else { 0 };
            let tile_h = if compact { 56.0 } else { tiles_h };
            let x = content.x0 + column as f64 * (tw + gap);
            let y = content.y0 + row as f64 * (tile_h + gap);
            let r = Rect::new(x, y, x + tw, y + tile_h);
            ui::surface::card(
                scene,
                t,
                r,
                f64::from(theme.tokens.radii.lg),
                theme.tokens.colors.surfaces.raised,
            );
            PaintCtx::new(scene, t, tr, theme).text(
                label,
                &TextStyle::new(10.0).with_weight(TextWeight::Bold),
                theme.text_dim,
                Point::new(r.x0 + 14.0, r.y0 + if compact { 8.0 } else { 14.0 }),
                None,
            );
            PaintCtx::new(scene, t, tr, theme).text(
                &ellipsize(val, 16),
                &TextStyle::new(17.0).with_weight(TextWeight::Semibold),
                theme.text_primary,
                Point::new(r.x0 + 14.0, r.y0 + if compact { 27.0 } else { 36.0 }),
                None,
            );
        }
    }

    /// The real measured-rate sparkline chart plus its legend/empty note.
    /// Returns the chart's rect so the caller can position what follows it.
    /// Split out of [`Self::draw_status_tab`].
    fn draw_status_throughput(
        ctx: &mut PaintCtx<'_>,
        chart: Rect,
        rates: &RateReadings,
        c: &Connection,
    ) {
        let (scene, t, tr, theme) = (&mut *ctx.scene, ctx.transform, &mut *ctx.text, ctx.theme);
        let (has_rate, latest) = (rates.live, rates.latest);
        // Throughput panel — plots the REAL measured rate series. When there
        // is no live data yet the plot stays empty with an honest note; the
        // app never draws a synthesized waveform. `chart` comes from
        // `status_geometry`, the rhythm the hit layout reads too.
        Self::draw_throughput_plot(scene, tr, t, theme, chart, &rates.down, &rates.up, has_rate);
        PaintCtx::new(scene, t, tr, theme).text(
            "Throughput",
            &TextStyle::new(12.0).with_weight(TextWeight::Bold),
            theme.text_dim,
            Point::new(chart.x0 + 12.0, chart.y0 + 10.0),
            None,
        );
        if let Some(x) = latest {
            let legend = format!(
                "recv {} · sent {}",
                ui::format::bytes(x.rx),
                ui::format::bytes(x.tx)
            );
            let (lw, _) = tr.measure_weighted(&legend, 11.0, None, TextWeight::Regular);
            PaintCtx::new(scene, t, tr, theme).text(
                &legend,
                &TextStyle::new(11.0),
                theme.text_dim,
                Point::new(chart.x1 - 12.0 - f64::from(lw), chart.y0 + 10.0),
                None,
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
            PaintCtx::new(scene, t, tr, theme).text(
                note,
                &TextStyle::new(12.0),
                theme.text_dim,
                Point::new(
                    chart.center().x - f64::from(nw) / 2.0,
                    chart.center().y - 8.0,
                ),
                None,
            );
        }
    }

    /// The throughput plot on its raised card: download (info) and upload
    /// (success) share one scale with headroom, each leading with a dot at the
    /// newest reading. Only ever plots the measured series it is handed —
    /// without a live rate the card shows its grid and nothing else.
    fn draw_throughput_plot(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        chart: Rect,
        down: &[f64],
        up: &[f64],
        live: bool,
    ) {
        let raised = theme.tokens.colors.surfaces.raised;
        // The plot is a raised card like the stat tiles above it, with the
        // edge derived from its own fill (HIG §9.5).
        ui::surface::card(scene, t, chart, f64::from(theme.tokens.radii.lg), raised);
        let plot = chart.inset(-12.0);
        if plot.width() <= 1.0 || plot.height() <= 1.0 {
            return;
        }
        let peak = down
            .iter()
            .chain(up)
            .copied()
            .fold(0.0_f64, f64::max)
            .max(1.0);
        let semantic = &theme.tokens.colors.semantic;
        let series = if live {
            let as_f32 = |rates: &[f64]| rates.iter().map(|&r| r as f32).collect();
            vec![
                Series::new(as_f32(down), semantic.info),
                Series::new(as_f32(up), semantic.success),
            ]
        } else {
            Vec::new()
        };
        let style = ChartStyle::default()
            .without_header()
            .with_grid(3, Some(ui::surface::edge_on(raised).multiply_alpha(0.6)))
            .with_trend(
                TrendStyle::CHART
                    .with_line_width(2.0)
                    .with_fill(ui::charts::TrendFill::Alpha(41))
                    .with_leading_dot(3.0),
            );
        Chart::draw_styled(
            &mut PaintCtx::new(scene, t, tr, theme),
            plot,
            &series,
            TrendRange::up_to(peak * 1.15),
            ChartLabels::default(),
            &style,
        );
    }

    /// The secret-free provisioning explanation plus the per-tunnel Tor
    /// toggle row. Split out of [`Self::draw_status_tab`].
    fn draw_status_kv_rows(
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        content: Rect,
        lay: &LayoutRects,
        c: &Connection,
        details: Option<&LocalProfileDetails>,
    ) {
        let (kv_top, row_h) = status_kv_table(content);
        let rows = [
            (
                "Endpoint",
                details.map_or_else(
                    || "Credentials unavailable on this device".into(),
                    |d| d.endpoint.clone(),
                ),
            ),
            ("DNS", details.map_or_else(|| "—".into(), |d| d.dns.clone())),
            (
                "Allowed IPs",
                details.map_or_else(|| "—".into(), |d| d.allowed_ips.clone()),
            ),
            (
                "Public key",
                details.map_or_else(|| "—".into(), |d| ellipsize(&d.public_key, 32)),
            ),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = kv_top + i as f64 * row_h;
            PaintCtx::new(scene, t, tr, theme).text(
                k,
                &TextStyle::new(12.0),
                theme.text_dim,
                Point::new(content.x0, ry + 6.0),
                None,
            );
            PaintCtx::new(scene, t, tr, theme).text(
                v,
                &TextStyle::new(12.0),
                theme.text_primary,
                Point::new(content.x0 + 130.0, ry + 6.0),
                None,
            );
            // Row rules derived against the pane they lie on (HIG §9.5).
            ui::surface::separator(
                scene,
                t,
                content.x0,
                content.x1,
                ry + row_h,
                0.0,
                theme.tokens.colors.surfaces.base,
            );
        }
        // Per-tunnel Tor row (with a small toggle).
        let ry = kv_top + 4.0 * row_h;
        PaintCtx::new(scene, t, tr, theme).text(
            "Route over Tor",
            &TextStyle::new(12.0),
            theme.text_dim,
            Point::new(content.x0, ry + 6.0),
            None,
        );
        Self::draw_switch(scene, t, theme, lay.tor_row, c.tor);
    }

    fn draw_config_tab(
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
        // A raised card with its derived edge, like the Status tiles.
        ui::surface::card(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.surfaces.raised,
        );
        let text = render_config_text(c, self.local_details.get(&c.id));
        let mut y = card.y0 + 14.0;
        for line in text.lines() {
            let col = if line.starts_with('[') || line.starts_with('<') {
                theme.tokens.colors.accent[6]
            } else if line.contains('=') || line.split_whitespace().count() > 1 {
                theme.text_primary
            } else {
                theme.text_dim
            };
            tr.draw_style(
                scene,
                line,
                col,
                t * Affine::translate((card.x0 + 16.0, y)),
                None,
                &TextStyle::new(12.5).with_family(FontChoice::Monospace),
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
        // A raised card with its derived edge, like the Status tiles.
        ui::surface::card(
            scene,
            t,
            card,
            f64::from(theme.tokens.radii.lg),
            theme.tokens.colors.surfaces.raised,
        );
        let xfer = self.throughput.get(&c.id).and_then(TransferHistory::latest);
        let lines = synth_logs(c, self.local_details.get(&c.id), &self.activity, xfer);
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
            tr.draw_style(
                scene,
                &ellipsize(line, 78),
                col,
                t * Affine::translate((card.x0 + 16.0, y)),
                None,
                &TextStyle::new(12.0).with_family(FontChoice::Monospace),
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
        // Panel role: the panel ground with its derived panel edge (HIG §9.5).
        let card = lay.modal_card;
        let radius = f64::from(theme.tokens.radii.xl);
        fill_round(scene, t, card, radius, panel_ground(theme));
        stroke_round(
            scene,
            t,
            card.inset(-0.5),
            radius - 0.5,
            panel_border(theme),
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
        PaintCtx::new(scene, t, tr, theme).text(
            if self.provision_target.is_some() {
                "Set up on this device"
            } else {
                "Import VPN profile"
            },
            &TextStyle::new(18.0).with_weight(TextWeight::Bold),
            theme.text_primary,
            Point::new(x, card.y0 + 24.0),
            None,
        );
        PaintCtx::new(scene, t, tr, theme).text(
            "Protocol",
            &TextStyle::new(11.0).with_weight(TextWeight::Bold),
            theme.text_dim,
            Point::new(x, card.y0 + 60.0),
            None,
        );
        self.draw_add_kind_selector(scene, tr, t, theme, card);
        Self::draw_add_fields(scene, tr, t, theme, presence, card);
        self.draw_add_modal_footer(scene, tr, t, theme, card);
    }

    /// The `WireGuard`/`OpenVPN` segmented protocol selector in the add
    /// modal. Split out of [`Self::draw_add_modal`] to keep that function's
    /// line count in check.
    fn draw_add_kind_selector(
        &self,
        scene: &mut Scene,
        tr: &mut TextRenderer,
        t: Affine,
        theme: &Theme,
        card: Rect,
    ) {
        // Shared segmented control over the one geometry the hit test reads.
        let rects = add_modal_kind_rects(card);
        let labels: Vec<String> = ADD_MODAL_KINDS
            .iter()
            .map(|k| format!("{} (.{})", k.long_name(), k.ext()))
            .collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let selected = ADD_MODAL_KINDS
            .iter()
            .position(|k| *k == self.add_kind)
            .unwrap_or(0);
        let hovered = rects.iter().position(|r| point_in(*r, self.last_mouse));
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(&rects, &labels, selected)
                .radius(f64::from(theme.tokens.radii.md))
                .text_size(12.0)
                .hovered(hovered),
        );
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
            let size = if weight == TextWeight::Semibold {
                13.0
            } else {
                11.0
            };
            PaintCtx::new(scene, t, tr, theme).text(
                line,
                &TextStyle::new(size).with_weight(weight),
                if weight == TextWeight::Semibold {
                    theme.text_primary
                } else {
                    // Explanatory copy remains secondary without dropping
                    // below the normal readable-on-raised-surface contrast.
                    theme.text_primary.multiply_alpha(0.78)
                },
                Point::new(x, y),
                None,
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
        PaintCtx::new(scene, t, tr, theme).text(
            "No profile is saved if validation or import fails.",
            &TextStyle::new(11.0),
            theme.text_dim,
            Point::new(card.x0 + 28.0, cancel.y0 - 24.0),
            None,
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
        // kind segments — the same rects the painter draws
        for (r, k) in add_modal_kind_rects(card).into_iter().zip(ADD_MODAL_KINDS) {
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

    /// Semantic mirror of the import sheet: the protocol segments, Cancel and
    /// the Import button, on the same rects the painter and hit test use.
    fn add_modal_access(&self, card: Rect) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, AccessState, Role};
        let mut children: Vec<AccessNode> = add_modal_kind_rects(card)
            .into_iter()
            .zip(ADD_MODAL_KINDS)
            .map(|(bounds, kind)| {
                let target = match kind {
                    ConnKind::WireGuard => "vpn:add-kind:wireguard",
                    ConnKind::OpenVpn => "vpn:add-kind:openvpn",
                };
                AccessNode::new(Role::RadioButton)
                    .with_name(kind.long_name())
                    .with_action_target(target)
                    .with_bounds(bounds)
                    .with_state(
                        AccessState::new()
                            .focusable(true)
                            .checked(self.add_kind == kind),
                    )
            })
            .collect();
        let (cancel, import) = add_modal_button_rects(card);
        children.push(
            AccessNode::new(Role::Button)
                .with_name("Cancel")
                .with_action_target("vpn:cancel-import")
                .with_bounds(cancel)
                .with_state(AccessState::new().focusable(true)),
        );
        let (import_target, import_name) = match self.add_kind {
            ConnKind::WireGuard => ("vpn:import-wireguard", "Import WireGuard profile"),
            ConnKind::OpenVpn => ("vpn:import-openvpn", "Import OpenVPN profile"),
        };
        children.push(
            AccessNode::new(Role::Button)
                .with_name(import_name)
                .with_action_target(import_target)
                .with_bounds(import)
                .with_state(AccessState::new().focusable(true)),
        );
        AccessNode::new(Role::Dialog)
            .with_name(if self.provision_target.is_some() {
                "Set up on this device"
            } else {
                "Import VPN profile"
            })
            .with_bounds(card)
            .with_children(children)
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

/// Protocol choices of the import sheet, in segment order.
const ADD_MODAL_KINDS: [ConnKind; 2] = [ConnKind::WireGuard, ConnKind::OpenVpn];

/// The protocol segments of the import sheet: one contiguous strip so the
/// shared segmented control paints it, and the only geometry the hit test
/// consults, so a face can never drift from its click target.
fn add_modal_kind_rects(card: Rect) -> [Rect; 2] {
    let seg_y = card.y0 + 78.0;
    let x = card.x0 + 28.0;
    let seg_w = 130.0;
    [0.0, 1.0].map(|i: f64| Rect::new(x + i * seg_w, seg_y, x + (i + 1.0) * seg_w, seg_y + 34.0))
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

fn render_config_text(c: &Connection, details: Option<&LocalProfileDetails>) -> String {
    let Some(details) = details else {
        return format!(
            "{} profile\n\nCredentials unavailable on this device.\n\nImport the exact provider profile to provision it securely through NetworkManager.\n\nRaw configuration is never synchronized or retained by VPN Studio.",
            c.kind.long_name()
        );
    };
    match c.kind {
        ConnKind::WireGuard => format!(
            "[Interface]\nPrivateKey = (stored securely)\nAddress = {}\nDNS = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {}",
            details.address,
            details.dns,
            ellipsize(&details.public_key, 32),
            details.endpoint,
            details.allowed_ips
        ),
        ConnKind::OpenVpn => format!(
            "client\nremote {}\n\n# routes: {}\n# dns: {}\n# credentials are stored securely by NetworkManager",
            details.endpoint, details.allowed_ips, details.dns
        ),
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
            "{} connection is active on this device",
            c.kind.long_name()
        ));
        if let Some(details) = details {
            out.push(format!("local endpoint {}", details.endpoint));
        }
        if let Some(x) = xfer {
            out.push(format!(
                "received {} · sent {}",
                ui::format::bytes(x.rx),
                ui::format::bytes(x.tx)
            ));
        }
        if c.tor {
            out.push("Tor routing preference is enabled".into());
        }
    } else {
        out.push("Connection is inactive on this device".into());
    }
    // append global activity newest-last
    for a in activity.iter().rev().take(8) {
        out.push(a.clone());
    }
    out
}

impl Widget for VpnApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[
            actions::VPN_TOGGLE_TYPE,
            actions::VPN_IMPORT_TYPE,
            actions::VPN_DELETE_TYPE,
            actions::VPN_PROVISION_TYPE,
        ]
    }

    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        let (state, _) = ui::doc::document(state, path);
        #[cfg(test)]
        if let Some(kind) = self.history_control_arm.as_ref().and_then(|arm| arm.take()) {
            self.queue_import(kind);
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

    /// Size the header control band from the label the renderer will paint,
    /// so an enlarged accessibility scale grows the band (and the tally that
    /// centres on it) instead of sinking the labels to the pill's edge.
    fn adjust_layout_with_measure(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        tr: &mut TextRenderer,
    ) -> bool {
        let (_, label_h) = tr.measure_weighted("+ Import", 13.0, None, TextWeight::Semibold);
        let control_h = HEADER_CONTROL_H.max(f64::from(label_h).ceil() + 16.0);
        // Rows: the 15px name line, the 12px endpoint line beneath it and the
        // paddings the 1× row implied (14 / 11.25 / 15).
        let (_, name_h) = tr.measure_weighted("Hg", 15.0, None, TextWeight::Semibold);
        let (_, line2_h) = tr.measure("Hg", 12.0, None);
        let row_line2_top = ROW_LINE2_TOP.max(14.0 + f64::from(name_h) + 11.25);
        let row_h = ROW_H.max(row_line2_top + f64::from(line2_h) + 15.0);
        let row_name_cy = 14.0 + f64::from(name_h) / 2.0;
        let changed = (control_h - self.control_h).abs() > f64::EPSILON
            || (row_h - self.row_h).abs() > f64::EPSILON
            || (row_line2_top - self.row_line2_top).abs() > f64::EPSILON
            || (row_name_cy - self.row_name_cy).abs() > f64::EPSILON;
        self.control_h = control_h;
        self.row_h = row_h;
        self.row_line2_top = row_line2_top;
        self.row_name_cy = row_name_cy;
        changed
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
        // A taller pane shrinks the scroll range under the current offset.
        let lay = self.layout();
        self.detail_scroll = self.detail_scroll.min(self.detail_scroll_max(&lay));
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
            if self.compact_detail_open || self.vault.connections.is_empty() {
                self.draw_detail(scene, text_renderer, transform, theme, &lay);
            } else {
                self.draw_list(scene, text_renderer, transform, theme, &lay);
            }
        } else {
            if self.vault.connections.is_empty() {
                self.draw_detail(scene, text_renderer, transform, theme, &lay);
            } else {
                self.draw_list(scene, text_renderer, transform, theme, &lay);
                self.draw_detail(scene, text_renderer, transform, theme, &lay);
            }
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if self.modal == Modal::Add {
            self.route_add_modal_event(event, ctx.scale_factor, ctx.handled);
            return;
        }
        // The Confirm dialog is modal: while it's up the shared ModalDialog
        // captures every event (clicks, Esc, y/n) and nothing leaks to the
        // list behind the scrim.
        if self.modal == Modal::Confirm {
            self.route_confirm_event(
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.handled,
            );
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, ctx.scale_factor, ctx.handled);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                self.handle_detail_scroll(delta, ctx.handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_click(ctx.state, ctx.handled);
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
                self.handle_key_press(logical_key, ctx.handled);
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "VpnApp"
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
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
            AccessNode::new(Role::Switch)
                .with_name("Route all VPN traffic over Tor")
                .with_action_target("vpn:tor-global")
                .with_bounds(lay.tor_pill)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.vault.tor_global),
                ),
            button("Import VPN profile", "vpn:import-dialog", lay.add_btn),
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
            if lay.compact && !self.vault.connections.is_empty() {
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
                        .with_name(match self.local_details.get(&connection.id) {
                            Some(details) => format!(
                                "{} {} profile. Local endpoint {}, address {}, DNS {}, routes {}, public key {}.",
                                connection.name, connection.kind.long_name(), details.endpoint,
                                details.address, details.dns, details.allowed_ips,
                                ellipsize(&details.public_key, 32)
                            ),
                            None => format!(
                                "{} {} profile. Credentials unavailable on this device; import the exact provider profile to provision it securely.",
                                connection.name, connection.kind.long_name()
                            ),
                        })
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
        // The import sheet is modal: while it is up it is the only surface a
        // pointer or keyboard can reach, so it is the only one published.
        if self.modal == Modal::Add {
            children = vec![self.add_modal_access(lay.modal_card)];
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
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(
            action,
            ui::a11y::Action::Activate | ui::a11y::Action::ToggleCheck
        ) {
            return false;
        }
        match target {
            "vpn:import-wireguard" | "vpn:import-openvpn" => {
                let kind = if target == "vpn:import-wireguard" {
                    ConnKind::WireGuard
                } else {
                    ConnKind::OpenVpn
                };
                if self.modal == Modal::Add {
                    // The sheet's Import button: same path as a click or Enter.
                    self.add_kind = kind;
                    self.commit_add();
                } else {
                    self.queue_import(kind);
                }
            }
            "vpn:import-dialog" => self.open_add(ConnKind::WireGuard),
            "vpn:add-kind:wireguard" if self.modal == Modal::Add => {
                self.add_kind = ConnKind::WireGuard;
            }
            "vpn:add-kind:openvpn" if self.modal == Modal::Add => {
                self.add_kind = ConnKind::OpenVpn;
            }
            "vpn:cancel-import" if self.modal == Modal::Add => self.modal = Modal::None,
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
                if self.local_credentials.contains(&id) {
                    self.request_toggle(id);
                } else {
                    self.request_local_provision(id);
                }
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

impl ui::resources::PublishesResources for VpnApp {
    /// Standalone / `--shots` / unit tests publish the full vault tree and
    /// overlay live NetworkManager availability on connect / disconnect /
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
        if self.actions.is_hosted() && id == "vpn.vault" {
            return None;
        }
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
    }
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.pending_control_invocation.take_legacy_compatibility()
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
        if lay.compact
            && !self.vault.connections.is_empty()
            && self.compact_detail_open
            && point_in(lay.back_btn, p)
        {
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
                if self.local_credentials.contains(&id) {
                    self.request_toggle(id);
                } else {
                    self.request_local_provision(id);
                }
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

    /// Wheel over the detail pane scrolls its tab content wherever the pane
    /// is shown — docked beside the list as well as in the compact route —
    /// so a short window (1024×640 with a stacked header) still reaches the
    /// Tor row at the bottom of the Status table.
    fn handle_detail_scroll(&mut self, delta: &MouseScrollDelta, handled: &mut bool) {
        let lay = self.layout();
        if !self.detail_shown(&lay) || !point_in(lay.detail, self.last_mouse) {
            return;
        }
        let delta_y = match delta {
            MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 32.0,
            MouseScrollDelta::PixelDelta(position) => position.y,
        };
        let max_scroll = self.detail_scroll_max(&lay);
        self.detail_scroll = (self.detail_scroll - delta_y).clamp(0.0, max_scroll);
        *handled = true;
    }

    /// Whether the detail pane is on screen: always beside the list when
    /// docked, only after a row was opened in the compact route.
    fn detail_shown(&self, lay: &LayoutRects) -> bool {
        self.vault.selected_conn().is_some() && (!lay.compact || self.compact_detail_open)
    }

    /// How far the current tab's content can scroll inside `lay.content`.
    /// The Status extent is the geometry the painter and the Tor-toggle hit
    /// rect share (`status_content_height`); Config and Logs paint a card
    /// clamped to the pane, so their fixed extents only matter once the pane
    /// is shorter than the card.
    fn detail_scroll_max(&self, lay: &LayoutRects) -> f64 {
        let content_height: f64 = match self.detail_tab {
            DetailTab::Status => status_content_height(lay.content),
            DetailTab::Config => 360.0,
            DetailTab::Logs => 420.0,
        };
        (content_height - lay.content.height()).max(0.0)
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

    /// The demo build populated + read back — used by the tests that need a
    /// fleet to operate on. The live app never fabricates this.
    fn demo_with_doc() -> (VpnApp, AutoCommit) {
        let mut app = VpnApp::new_demo();
        let mut doc = AutoCommit::new();
        app.refresh(&mut doc);
        (app, doc)
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
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn vpn_layout_scales_scrolls_and_keeps_controls_in_band() {
        // Case `docked_detail_scrolls_the_tor_row_into_view_at_1024x640`:
        // At 1024×640 the docked detail pane is 536px wide, so its header
        // stacks (the 150px action cannot share the title row) and the Status
        // table's Tor row lands below the pane. The pane scrolls — docked as
        // well as compact — by the extent the painter, the Tor hit rect and
        // `status_content_height` share, so the row is reachable with the wheel.
        {
            let (mut app, _doc) = demo_with_doc();
            if app.vault.selected.is_none() {
                let first = app.vault.connections[0].id;
                app.select(first);
            }
            app.detail_tab = DetailTab::Status;
            app.rect = Rect::new(0.0, 0.0, 1024.0, 640.0);
            let lay = app.layout();
            assert!(!lay.compact);
            assert!(
                lay.tor_row.y1 > lay.content.y1,
                "without scrolling the Tor row starts below the pane"
            );
            let extent = lay.content.y0 + status_content_height(lay.content);
            assert!(
                lay.tor_row.y1 <= extent + 1e-6,
                "the Tor row lies inside the Status extent"
            );

            app.last_mouse = lay.content.center();
            let mut handled = false;
            app.handle_detail_scroll(&MouseScrollDelta::LineDelta(0.0, -40.0), &mut handled);
            assert!(handled, "the docked pane takes the wheel");
            let max = app.detail_scroll_max(&lay);
            assert!(
                max > 0.0 && (app.detail_scroll - max).abs() < 1e-6,
                "clamped to the content extent"
            );
            assert!(
                lay.tor_row.y1 - app.detail_scroll <= lay.content.y1 + 1e-6,
                "the Tor row is inside the pane after scrolling"
            );

            // A tall window shows the whole table: nothing to scroll.
            app.rect = Rect::new(0.0, 0.0, 1024.0, 900.0);
            let tall = app.layout();
            assert!(tall.tor_row.y1 <= tall.content.y1);
            assert!(app.detail_scroll_max(&tall).abs() < 1e-6);
        }
        // Case `connection_rows_follow_the_text_scale`:
        // Connection rows keep their 74px band at 1× and grow with the measured
        // name + endpoint lines at 2×; the rows the layout builds (and hit-tests)
        // never overlap and the second line always clears the first.
        {
            let (mut app, _doc) = demo_with_doc();
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            assert!((app.row_h - ROW_H).abs() < 1e-6, "1× keeps the 74px row");
            assert!((app.row_line2_top - ROW_LINE2_TOP).abs() < 1e-6);
            let rows = app.layout().rows;
            assert!(rows.len() > 1);
            assert!(
                rows.iter()
                    .all(|(_, r, _)| (r.height() - ROW_H).abs() < 1e-6)
            );

            tr.set_text_scale(2.0);
            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let (_, name_h) = tr.measure_weighted("Hg", 15.0, None, TextWeight::Semibold);
            let (_, line2_h) = tr.measure("Hg", 12.0, None);
            assert!(
                app.row_line2_top >= 14.0 + f64::from(name_h),
                "the endpoint line starts under the doubled name line"
            );
            assert!(app.row_h >= app.row_line2_top + f64::from(line2_h));
            let rows = app.layout().rows;
            assert!(
                rows.iter()
                    .all(|(_, r, _)| (r.height() - app.row_h).abs() < 1e-6)
            );
            for pair in rows.windows(2) {
                assert!(pair[1].1.y0 >= pair[0].1.y1, "2× rows never overlap");
            }
            // The toggle stays centred in the taller row.
            let (_, row, toggle) = rows[0];
            assert!((toggle.center().y - row.center().y).abs() < 1e-6);
        }
        // Case `narrow_header_ends_after_controls_and_keeps_notice_actions_in_band`.
        {
            let (mut app, mut doc) = demo_with_doc();
            app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
            let clean = app.layout();
            assert!(clean.header.y1 - clean.add_btn.y1 <= 16.0);
            assert!(clean.rows[0].1.y0 >= clean.header.y1);
            app.shot_loading = true;
            let busy = app.layout();
            let notice = busy.notice.unwrap();
            assert!(notice.y0 >= busy.add_btn.y1);
            for target in [busy.notice_retry.unwrap(), busy.notice_dismiss.unwrap()] {
                assert!(notice.contains(target.origin()) && target.y1 <= notice.y1);
            }
            assert!(busy.rows[0].1.y0 >= notice.y1);
            app.shot_loading = false;
            let p = app.layout().tor_pill.center();
            let before = app.vault.tor_global;
            dispatch(&mut app, &mut doc, &cursor_at(p));
            dispatch(&mut app, &mut doc, &press());
            assert!(matches!(app.pending, Some(Pending::TorGlobal(next, _)) if next != before));
            dispatch(&mut app, &mut doc, &esc_key());
            assert!(app.pending.is_none());
            assert_eq!(app.vault.tor_global, before);
        }
        // Case `header_control_band_grows_with_the_text_scale`:
        // The header control band follows the measured Import label: at 2× it is
        // taller than the 1× constant and the layout's control rects grow with
        // it, so the tally and pill labels stay centred inside their band.
        {
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let node = taffy.new_leaf(Style::default()).unwrap();
            let mut tr = TextRenderer::new();
            let (mut app, _doc) = demo_with_doc();
            app.rect = Rect::new(0.0, 0.0, 480.0, 900.0);
            app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
            let base = app.layout();
            assert!((base.add_btn.height() - HEADER_CONTROL_H).abs() < 1e-6);

            tr.set_text_scale(2.0);
            assert!(app.adjust_layout_with_measure(&mut taffy, node, &mut tr));
            let large = app.layout();
            assert!(large.add_btn.height() > HEADER_CONTROL_H + 8.0);
            assert!(large.header.height() > base.header.height());
            assert!(
                large.list.y0 >= large.header.y1,
                "body starts below the grown header"
            );

            // The wide header must also contain the grown band, at 2× and at the
            // renderer's 4× clamp; before, only the compact form grew.
            app.rect = Rect::new(0.0, 0.0, 1200.0, 800.0);
            for scale in [2.0, 4.0] {
                tr.set_text_scale(scale);
                app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
                let wide = app.layout();
                assert!(!wide.compact);
                assert!(
                    wide.add_btn.y1 + 18.0 <= wide.header.y1 + 1e-6,
                    "controls end inside the wide header at {scale}x"
                );
                assert!(wide.tor_pill.y1 <= wide.header.y1);
            }
        }
        // Case `narrow_empty_state_has_no_dead_back_accessibility_action`.
        {
            fn has_back(node: &ui::a11y::AccessNode) -> bool {
                node.action_target.as_deref() == Some("vpn:back")
                    || node.children.iter().any(has_back)
            }
            let mut app = VpnApp::new();
            app.seed_shot_state("empty");
            app.rect = Rect::new(0.0, 0.0, 390.0, 844.0);
            let root = Widget::accessibility(&app).expect("VPN exports accessibility");
            assert!(
                !has_back(&root),
                "empty state has nowhere to navigate back to"
            );
        }
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
    fn control_custody_vpn_unreachable_private_route_saves_the_armed_import_and_confirms_a_fresh_one()
     {
        let (mut app, doc) = demo_with_doc();
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

        // The published import sheet, opened and committed semantically.
        driver
            .perform_semantic_named(
                "vpn_app",
                Some("vpn:import-dialog"),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
        driver
            .perform_semantic_named(
                "vpn_app",
                Some("vpn:import-wireguard"),
                &ui::a11y::Action::Activate,
            )
            .unwrap();
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
        assert_eq!(driver.control_coverage().registered, 1);
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
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &modifiers,
                state: doc,
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
    }

    #[test]
    #[serial_test::serial(vpn_env)]
    fn vpn_connection_transitions_are_honest_and_dismissable() {
        // Case `confirm_dialog_routes_through_modaldialog`:
        // The action confirm routes through the shared `ModalDialog`: Escape
        // dismisses it, and a click on its primary button runs the real
        // `confirm_pending` — both driven through the production `handle_event`.
        {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.remove("TILO_VPN_ALLOW_EXEC");
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
            let (mut app, mut doc) = demo_with_doc();
            app.operation_error = Some("The profile is incomplete".into());
            app.retry_import_kind = Some(ConnKind::WireGuard);

            dispatch(&mut app, &mut doc, &esc_key());

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
        // Case `exported_tunnel_toggle_is_a_cancellable_background_job`.
        {
            let (mut app, mut doc) = demo_with_doc();
            let connection = app.vault.connections[0].id;
            let invocation = tilo_actions::InvocationId::new();
            let started = app
                .start_operation(
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
                app.poll_resource_operation(invocation, true, &doc, &automerge::ROOT),
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
            app.seed_shot_state("unprovisioned");
            let id = app.vault.selected.expect("selected remote profile");
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
            for action_id in [
                if app.vault.get(id).is_some_and(Connection::is_connected) {
                    "disconnect"
                } else {
                    "connect"
                },
                "delete",
            ] {
                let action = profile
                    .actions
                    .iter()
                    .find(|action| action.id == action_id)
                    .unwrap();
                assert!(
                    matches!(action.contract.availability, tilo_actions::Availability::Unavailable { ref code, .. } if code == "local_credentials_required")
                );
            }
        }
    }

    #[test]
    fn add_modal_hit_tests_painted_controls_and_never_commits_unprovisioned() {
        // Case `add_modal_never_commits_an_unprovisioned_connection`.
        {
            let (mut app, _doc) = demo_with_doc();
            let before = app.vault.connections.len();
            app.open_add(ConnKind::OpenVpn);
            app.commit_add();
            assert_eq!(app.vault.connections.len(), before);
            assert!(
                app.pending_control_invocation
                    .controlled_pending_for_test()
                    .is_some()
            );
        }
        // Case `add_modal_kind_segments_hit_where_they_are_painted`:
        // The protocol segments are painted and hit-tested from one geometry:
        // pressing inside the second painted segment selects `OpenVPN`, the strip
        // is contiguous (so the shared segmented control can draw it) and stays
        // inside the sheet at the narrowest supported width.
        {
            for width in [1200.0, 480.0] {
                let (mut app, _doc) = demo_with_doc();
                app.rect = Rect::new(0.0, 0.0, width, 800.0);
                app.open_add(ConnKind::WireGuard);
                let lay = app.layout();
                let rects = add_modal_kind_rects(lay.modal_card);
                assert!(
                    (rects[0].x1 - rects[1].x0).abs() < 1e-6,
                    "segments are contiguous"
                );
                assert!(
                    rects[1].x1 <= lay.modal_card.x1 - 28.0 + 1e-6,
                    "strip stays in the card"
                );
                app.add_modal_hit(rects[1].center(), &lay);
                assert_eq!(app.add_kind, ConnKind::OpenVpn);
                app.add_modal_hit(rects[0].center(), &lay);
                assert_eq!(app.add_kind, ConnKind::WireGuard);
            }
        }
        // Case `add_modal_shared_button_rects_drive_save_and_cancel`.
        {
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
            assert!(
                app.pending_control_invocation
                    .controlled_pending_for_test()
                    .is_some()
            );
            assert_eq!(app.modal, Modal::None);

            app.open_add(ConnKind::OpenVpn);
            let card = app.layout().modal_card;
            let (cancel, _) = add_modal_button_rects(card);
            dispatch(&mut app, &mut doc, &cursor_at(cancel.center()));
            dispatch(&mut app, &mut doc, &press());
            assert_eq!(app.vault.connections.len(), before);
            assert_eq!(app.modal, Modal::None, "Cancel dismisses without saving");
        }
        // Case `add_modal_does_not_capture_credentials_and_enter_queues_import`.
        {
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
            assert!(
                app.pending_control_invocation
                    .controlled_pending_for_test()
                    .is_some()
            );
        }
    }
}
