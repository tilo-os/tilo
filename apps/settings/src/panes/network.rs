//! Network pane — Wi-Fi radio toggle, scanned network list, refresh
//! button, connection-status row.
//!
//! Plumbs `os_actions::network::{get_wifi, set_wifi,
//! list_wifi_networks, connect_wifi}`. The network list is sampled once
//! on construction (cached) and refreshed when the user clicks the
//! `Refresh` button — re-scanning every frame would be wasteful and
//! visibly stutter the UI on slow nmcli backends.
//
// CRDT keys:
//   `network.wifi_enabled` — bool, wifi toggle
//   `network.refresh`      — bool, click sets true; pane resets on
//                             sync after rescan
//   `network.connect.<ssid>` — bool, click sets true; pane resets on
//                             sync after dispatch

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use automerge::{transaction::Transactable, AutoCommit, ObjId, ReadDoc, Value};
use os_actions::{Dispatcher, WifiNetworkInfo, WiredInfo};
use taffy::{NodeId, TaffyTree};
use tracing::warn;
use ui::theme::Theme;
use ui::widget::{Button, Row, Spacer, Text, TextInput, Toggle, Widget};
use vello::kurbo::{Affine, Point, Rect};
use vello::Scene;
use winit::event::{Modifiers, WindowEvent};

use crate::dispatcher::dispatcher_or_real;
use crate::widgets::{card, card_with_action, caption, rows, value_row, El};

pub const WIFI_ENABLED_KEY: &str = "network.wifi_enabled";
pub const REFRESH_KEY: &str = "network.refresh";
/// CRDT key holding the SSID whose inline password prompt is expanded
/// (empty string = none). Kept in the document, not a pane field, so the
/// expansion survives the host's structure rebuilds (which reconstruct the
/// pane from scratch).
pub const EXPANDED_KEY: &str = "network.expanded";

/// Build the per-SSID "connect" CRDT key. A click on a network's primary
/// button writes this `true`; the pane reads it in `sync` and either
/// connects directly (open network) or expands an inline password prompt
/// (secure network).
fn connect_key(ssid: &str) -> String {
    format!("network.connect.{ssid}")
}

/// Per-SSID "join with password" CRDT key — written `true` by the Join
/// button in the inline password prompt.
fn join_key(ssid: &str) -> String {
    format!("network.join.{ssid}")
}

/// Per-SSID password-field CRDT key. The secret `TextInput` for the
/// expanded network writes its (masked) value here; `sync` reads it back
/// to pass to `connect_wifi`.
fn password_key(ssid: &str) -> String {
    format!("network.password.{ssid}")
}

/// Nerd Font (Material Design Icons) Wi-Fi signal-strength glyph for a 0..=4
/// bar level. These are the same MDI `wifi-strength-*` family the sidebar's
/// Network icon draws, so they render as real signal arcs. The previous
/// Unicode block-element bars (`▁▂▄▆█`) have no glyph in the shipped fonts and
/// fell back to a `.notdef` tofu box, which read as broken and conveyed no
/// signal level.
fn wifi_signal_glyph(signal: u8) -> &'static str {
    match signal {
        0 => "\u{f092b}", // wifi-strength-outline (no bars)
        1 => "\u{f091f}", // wifi-strength-1
        2 => "\u{f0922}", // wifi-strength-2
        3 => "\u{f0925}", // wifi-strength-3
        _ => "\u{f0928}", // wifi-strength-4 (full)
    }
}

/// Read a boolean scalar at `key`, defaulting to `false`.
fn read_bool(state: &AutoCommit, root: &ObjId, key: &str) -> bool {
    matches!(
        state.get(root, key),
        Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
    )
}

/// Read a string value at `key` — handles both a plain scalar string and
/// an Automerge `Text` object (which is what `TextInput` writes through).
fn read_str(state: &AutoCommit, root: &ObjId, key: &str) -> Option<String> {
    match state.get(root, key) {
        Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
            automerge::ScalarValue::Str(text) => Some(text.to_string()),
            _ => None,
        },
        Ok(Some((Value::Object(automerge::ObjType::Text), obj))) => state.text(&obj).ok(),
        _ => None,
    }
}

/// Network-list provider — the prod impl shells out to nmcli, tests
/// supply a stub closure. Boxed so the pane holds it as a trait object;
/// the box satisfies `Send + Sync` because the closure captures nothing
/// (or only thread-safe state).
type NetworkLister = Arc<dyn Fn() -> Result<Vec<WifiNetworkInfo>, String> + Send + Sync>;

/// A configured VPN / WireGuard connection and whether it is currently up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VpnInfo {
    pub name: String,
    pub active: bool,
}

/// VPN-list provider — prod shells out to `nmcli`, tests supply a closure.
type VpnLister = Arc<dyn Fn() -> Vec<VpnInfo> + Send + Sync>;

/// Read the configured VPN connections from NetworkManager. Two `nmcli`
/// calls: every connection (to find the VPN/WireGuard ones) and the active
/// set (to mark which are up). Best-effort — an empty vec when `nmcli` is
/// missing.
fn read_vpn_connections() -> Vec<VpnInfo> {
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("nmcli").args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    let all = match run(&["-t", "-f", "NAME,TYPE", "connection", "show"]) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let active = run(&["-t", "-f", "NAME", "connection", "show", "--active"]).unwrap_or_default();
    parse_vpn_connections(&all, &active)
}

/// Parse the two `nmcli -t` outputs into [`VpnInfo`]s. `all` lines are
/// `NAME:TYPE`; a connection is a VPN when its type is `vpn` or
/// `wireguard`. `active` lines are bare `NAME`s of up connections.
pub fn parse_vpn_connections(all: &str, active: &str) -> Vec<VpnInfo> {
    let active_names: Vec<&str> = active.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).collect();
    let mut out = Vec::new();
    for line in all.lines() {
        // NAME may itself contain no colon in practice; split on the LAST
        // colon so a name is preserved even if it had one (nmcli escapes it,
        // but be defensive).
        let Some((name, ty)) = line.rsplit_once(':') else {
            continue;
        };
        let ty = ty.trim();
        if ty != "vpn" && ty != "wireguard" {
            continue;
        }
        let name = name.trim().to_string();
        if name.is_empty() {
            continue;
        }
        let active = active_names.contains(&name.as_str());
        out.push(VpnInfo { name, active });
    }
    out
}

pub struct NetworkPane {
    dispatcher: Arc<dyn Dispatcher + Send + Sync>,
    lister: NetworkLister,
    /// Cached current network list — refreshed at construction and on
    /// `Refresh` button press.
    networks: Vec<WifiNetworkInfo>,
    /// Cached wired-connection snapshot, sampled at construction and on
    /// refresh.
    wired: WiredInfo,
    /// Configured VPN connections (read-only status), sampled at
    /// construction and on refresh.
    vpns: Vec<VpnInfo>,
    vpn_lister: VpnLister,
    initial_wifi_enabled: bool,
    last_wifi_enabled: bool,
    needs_seed: bool,
    /// Track which `network.join.<ssid>` keys we already dispatched
    /// for, so a single button press doesn't re-fire on every frame.
    dispatched_connects: Vec<String>,
    /// The secure SSID whose inline password prompt is currently expanded,
    /// if any. Mirrors [`EXPANDED_KEY`] in the document; clicking "Connect"
    /// on a secure network opens it; clicking it again, "Join", or a
    /// successful connect closes it.
    expanded_ssid: Option<String>,
    /// Set when `expanded_ssid` changed this `sync`, so the host re-runs
    /// `build_layout` and the inline password prompt appears/disappears.
    structure_dirty: bool,
    composed: Option<Box<dyn Widget>>,
    rect: Rect,
}

impl Default for NetworkPane {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkPane {
    pub fn new() -> Self {
        let lister: NetworkLister = Arc::new(|| os_actions::list_wifi_networks());
        let networks = lister().unwrap_or_default();
        let initial_wifi = os_actions::get_wifi().unwrap_or(true);
        let wired = os_actions::get_wired().unwrap_or_default();
        let vpn_lister: VpnLister = Arc::new(read_vpn_connections);
        let vpns = vpn_lister();
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            networks,
            wired,
            vpns,
            vpn_lister,
            initial_wifi_enabled: initial_wifi,
            last_wifi_enabled: initial_wifi,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            expanded_ssid: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test-only constructor that pins the dispatcher, the wifi-state,
    /// and the network-list source. The list is captured into the
    /// pane's cache immediately.
    pub fn with_dispatcher(
        dispatcher: Arc<dyn Dispatcher + Send + Sync>,
        initial_wifi_enabled: bool,
        networks: Vec<WifiNetworkInfo>,
    ) -> Self {
        let snapshot = networks.clone();
        let lister: NetworkLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher,
            lister,
            networks,
            wired: WiredInfo::default(),
            vpns: Vec::new(),
            vpn_lister: Arc::new(|| Vec::<VpnInfo>::new()),
            initial_wifi_enabled,
            last_wifi_enabled: initial_wifi_enabled,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            expanded_ssid: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// `--shots` helper: a pane with a fixed network list and the real
    /// dispatcher (clicks would shell out, but the render never clicks).
    pub fn new_demo(networks: Vec<WifiNetworkInfo>) -> Self {
        let snapshot = networks.clone();
        let lister: NetworkLister = Arc::new(move || Ok(snapshot.clone()));
        Self {
            dispatcher: dispatcher_or_real(None),
            lister,
            networks,
            wired: WiredInfo::default(),
            vpns: Vec::new(),
            vpn_lister: Arc::new(|| Vec::<VpnInfo>::new()),
            initial_wifi_enabled: true,
            last_wifi_enabled: true,
            needs_seed: true,
            dispatched_connects: Vec::new(),
            expanded_ssid: None,
            structure_dirty: false,
            composed: None,
            rect: Rect::ZERO,
        }
    }

    /// Test/`--shots` helper: pin the wired snapshot so the Wired card
    /// renders deterministic content.
    pub fn with_wired(mut self, wired: WiredInfo) -> Self {
        self.wired = wired;
        self
    }

    /// Test/`--shots` helper: pin the VPN list so the VPN card renders
    /// deterministic content. Also pins the refresh lister to the same set.
    pub fn with_vpns(mut self, vpns: Vec<VpnInfo>) -> Self {
        let snapshot = vpns.clone();
        self.vpn_lister = Arc::new(move || snapshot.clone());
        self.vpns = vpns;
        self
    }

    pub fn vpns(&self) -> &[VpnInfo] {
        &self.vpns
    }

    /// Rows for the VPN card — a read-only status list of configured
    /// profiles. Connecting/disconnecting needs a privileged NetworkManager
    /// action seam that isn't in this app yet, so the card reports status
    /// truthfully rather than offering a button that can't act.
    fn vpn_rows(&self) -> Vec<El> {
        if self.vpns.is_empty() {
            return vec![caption(
                "No VPN profiles configured. Add a VPN connection to see its \
                 status here.",
            )];
        }
        let mut out: Vec<El> = Vec::new();
        for v in &self.vpns {
            out.push(value_row(
                &v.name,
                if v.active { "Connected" } else { "Disconnected" },
            ));
        }
        out.push(caption("VPN profiles are managed with your network connections."));
        out
    }

    /// Test/`--shots` helper: pre-expand a network's inline password prompt
    /// so the deterministic render shows it. Also forces the radio "on" so
    /// the network list renders.
    pub fn with_expanded(mut self, ssid: &str) -> Self {
        self.expanded_ssid = Some(ssid.to_string());
        self.last_wifi_enabled = true;
        self.initial_wifi_enabled = true;
        self
    }

    /// Cached network list. Tests read this to assert the panes
    /// rendered the right rows.
    pub fn networks(&self) -> &[WifiNetworkInfo] {
        &self.networks
    }

    fn connection_status_text(&self) -> String {
        if !self.last_wifi_enabled {
            return "Wi-Fi is off".to_string();
        }
        match self.networks.iter().find(|n| n.connected) {
            Some(n) => format!("Connected to {}", n.ssid),
            None => "Not connected".to_string(),
        }
    }

    fn build_composition(&self) -> Box<dyn Widget> {
        // --- Wi-Fi card: header carries the toggle; body is status +
        //     scanned-network rows + a Refresh action.
        let wifi_toggle: El = Box::new(
            Toggle::new("settings.network.wifi", WIFI_ENABLED_KEY).with_id("network.wifi"),
        );

        let mut wifi_rows: Vec<El> = Vec::new();
        wifi_rows.push(value_row("Status", &self.connection_status_text()));

        if self.last_wifi_enabled {
            for net in &self.networks {
                wifi_rows.push(self.network_row(net));
                // Inline password prompt expands directly beneath the row
                // whose "Connect" was pressed (secure networks only).
                if self.expanded_ssid.as_deref() == Some(net.ssid.as_str()) {
                    wifi_rows.push(self.password_prompt(&net.ssid));
                }
            }
        }
        wifi_rows.push(Box::new(
            Row::new()
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(
                    "settings.network.refresh",
                    "\u{f0450}  Refresh",
                    Some(REFRESH_KEY),
                ))),
        ));
        wifi_rows.push(caption(
            "Wi-Fi scanning and connection are best-effort via nmcli; \
             secure networks prompt for a password before joining.",
        ));

        let wifi_card = card_with_action("Wi-Fi", wifi_toggle, rows(wifi_rows));

        // --- Wired card (real nmcli snapshot).
        let wired_card = card("Wired", rows(self.wired_rows()));

        // --- VPN card (real read-only status of configured profiles).
        let vpn_card = card("VPN", rows(self.vpn_rows()));

        crate::widgets::cards(vec![wifi_card, wired_card, vpn_card])
    }

    /// One scanned-network row: signal + lock glyph + SSID on the left, a
    /// Connect / Connected button on the right.
    fn network_row(&self, net: &WifiNetworkInfo) -> El {
        let signal_glyph = wifi_signal_glyph(net.signal);
        // A closed padlock for secure networks, its open-padlock pair for open
        // ones — both from the MDI `lock` family the shipped fonts carry, so the
        // security state reads at a glance without a stray unrelated glyph.
        let lock_glyph = if net.secure { "\u{f033e}" } else { "\u{f033f}" };
        let label = format!("{signal_glyph}  {lock_glyph}  {ssid}", ssid = net.ssid);
        let key = connect_key(&net.ssid);
        let cta_id = format!("settings.network.connect.{}", net.ssid);
        let cta_label = if net.connected {
            "Connected"
        } else if self.expanded_ssid.as_deref() == Some(net.ssid.as_str()) {
            "Cancel"
        } else {
            "Connect"
        };
        Box::new(
            Row::new()
                .push(Box::new(Text::new(&label).with_size(13.0)))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(&cta_id, cta_label, Some(&key)))),
        )
    }

    /// The inline password prompt shown beneath a secure network when its
    /// "Connect" is pressed: a masked field bound to the per-SSID password
    /// key plus a Join button that writes the per-SSID join key.
    fn password_prompt(&self, ssid: &str) -> El {
        let pw_key = password_key(ssid);
        let jk = join_key(ssid);
        let field_id = format!("settings.network.password.{ssid}");
        let join_id = format!("settings.network.join.{ssid}");
        let field = TextInput::new(Vec::new())
            .with_id(&field_id)
            .with_on_change_path(&pw_key)
            .with_secret(true)
            .with_placeholder("Password")
            .with_font_size(13.0);
        Box::new(
            Row::new()
                .push(Box::new(field))
                .push(Box::new(Spacer::new()))
                .push(Box::new(Button::new(&join_id, "Join", Some(&jk)))),
        )
    }

    /// Rows for the Wired card, derived from the live `nmcli` snapshot.
    fn wired_rows(&self) -> Vec<El> {
        if !self.wired.connected {
            return vec![
                value_row("Ethernet", "Not connected"),
                caption("Plug in an Ethernet cable to use a wired connection."),
            ];
        }
        let mut out = Vec::new();
        let status = match &self.wired.speed {
            Some(s) => format!("Connected · {s}"),
            None => "Connected".to_string(),
        };
        out.push(value_row("Ethernet", &status));
        if let Some(dev) = &self.wired.device {
            out.push(value_row("Interface", dev));
        }
        out.push(value_row(
            "IPv4",
            self.wired.ipv4.as_deref().unwrap_or("—"),
        ));
        out
    }

    /// Update which network's password prompt is expanded, mirroring it
    /// into the document and flagging a structure rebuild so the prompt
    /// appears/disappears on the next frame.
    fn set_expanded(&mut self, state: &mut AutoCommit, root: &ObjId, ssid: Option<String>) {
        if self.expanded_ssid == ssid {
            return;
        }
        let _ = state.put(root, EXPANDED_KEY, ssid.clone().unwrap_or_default());
        // Seed the password field's Text object so it is typeable as soon as
        // the prompt appears (a `TextInput` persists edits only through an
        // existing Text object).
        if let Some(s) = &ssid {
            let _ = crate::ensure_text_object(state, root, &password_key(s));
        }
        self.expanded_ssid = ssid;
        self.structure_dirty = true;
    }

    /// Dispatch a Wi-Fi connection, deduplicated per SSID so a single
    /// button press doesn't re-fire on every frame.
    fn do_connect(&mut self, ssid: &str, password: Option<&str>) {
        if self.dispatched_connects.iter().any(|s| s == ssid) {
            return;
        }
        self.dispatched_connects.push(ssid.to_string());
        if let Err(e) = self.dispatcher.connect_wifi(ssid, password) {
            warn!(target: "qos::settings", op = "connect_wifi", error = %e, "network pane dispatch failed");
        }
    }
}

pub fn build() -> Box<dyn Widget> {
    Box::new(NetworkPane::new())
}

impl Widget for NetworkPane {
    fn sync(&mut self, state: &mut AutoCommit, root: &ObjId) {
        if self.needs_seed {
            // Seed only keys the document doesn't already carry, so a
            // structure rebuild (which reconstructs this pane and re-runs
            // the seed) never clobbers a value the user already changed —
            // e.g. toggling Wi-Fi off and then expanding a password prompt.
            if state.get(root, WIFI_ENABLED_KEY).ok().flatten().is_none() {
                let _ = state.put(
                    root,
                    WIFI_ENABLED_KEY,
                    automerge::ScalarValue::Boolean(self.initial_wifi_enabled),
                );
            } else {
                // Adopt the existing value so the dispatcher-change guard
                // doesn't re-fire `set_wifi` on the first post-rebuild sync.
                if let Ok(Some((Value::Scalar(s), _))) = state.get(root, WIFI_ENABLED_KEY) {
                    if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                        self.last_wifi_enabled = *b;
                    }
                }
            }
            let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
            // Seed the expansion mirror from the field so a pre-expanded
            // pane (tests / `--shots`) keeps its prompt open through the
            // first sync (which would otherwise read the empty doc as "no
            // prompt" and collapse it). Only when nothing's there yet.
            if state.get(root, EXPANDED_KEY).ok().flatten().is_none() {
                let _ = state.put(
                    root,
                    EXPANDED_KEY,
                    self.expanded_ssid.clone().unwrap_or_default(),
                );
            }
            // A pre-expanded pane (tests / `--shots`) needs its password field's
            // Text object seeded so the field is typeable / renders its value.
            if let Some(ssid) = self.expanded_ssid.clone() {
                let _ = crate::ensure_text_object(state, root, &password_key(&ssid));
            }
            self.needs_seed = false;
        }

        // Wi-Fi toggle.
        if let Ok(Some((val, _))) = state.get(root, WIFI_ENABLED_KEY) {
            if let Value::Scalar(s) = val {
                if let automerge::ScalarValue::Boolean(b) = s.as_ref() {
                    if *b != self.last_wifi_enabled {
                        self.last_wifi_enabled = *b;
                        if let Err(e) = self.dispatcher.set_wifi(*b) {
                            warn!(target: "qos::settings", op = "set_wifi", error = %e, "network pane dispatch failed");
                        }
                    }
                }
            }
        }

        // Refresh button — when the button writes `true`, rescan and
        // clear the bit so subsequent frames don't loop.
        let refresh_pressed = matches!(
            state.get(root, REFRESH_KEY),
            Ok(Some((Value::Scalar(s), _))) if matches!(s.as_ref(), automerge::ScalarValue::Boolean(true))
        );
        if refresh_pressed {
            let _ = state.put(root, REFRESH_KEY, automerge::ScalarValue::Boolean(false));
            match (self.lister)() {
                Ok(nets) => self.networks = nets,
                Err(e) => warn!(target: "qos::settings", op = "list_wifi_networks", error = %e, "rescan failed"),
            }
            self.vpns = (self.vpn_lister)();
        }

        // Restore the expanded-prompt SSID from the document so it
        // survives the host's structure rebuilds (which reconstruct this
        // pane). An empty string means "no prompt open".
        let doc_expanded = read_str(state, root, EXPANDED_KEY).filter(|s| !s.is_empty());
        if doc_expanded != self.expanded_ssid {
            self.expanded_ssid = doc_expanded;
        }

        // Snapshot the network attributes we need without holding a borrow
        // across the mutating `state` calls below.
        let nets: Vec<(String, bool)> = self
            .networks
            .iter()
            .map(|n| (n.ssid.clone(), n.secure))
            .collect();

        // Per-network "Connect" buttons. Open networks connect immediately;
        // secure networks toggle the inline password prompt instead.
        for (ssid, secure) in &nets {
            let key = connect_key(ssid);
            let pressed = read_bool(state, root, &key);
            if !pressed {
                continue;
            }
            let _ = state.put(root, key.as_str(), automerge::ScalarValue::Boolean(false));
            if *secure {
                // Toggle this network's prompt: collapse if it's already
                // the open one, otherwise open it (closing any other).
                let now = if self.expanded_ssid.as_deref() == Some(ssid.as_str()) {
                    None
                } else {
                    Some(ssid.clone())
                };
                self.set_expanded(state, root, now);
            } else {
                // Open network — connect with no password.
                self.do_connect(ssid, None);
                self.set_expanded(state, root, None);
            }
        }

        // Per-network "Join" buttons in the expanded password prompt:
        // connect with the typed password, then collapse the prompt.
        for (ssid, _secure) in &nets {
            let jk = join_key(ssid);
            if !read_bool(state, root, &jk) {
                continue;
            }
            let _ = state.put(root, jk.as_str(), automerge::ScalarValue::Boolean(false));
            let pw = read_str(state, root, &password_key(ssid)).filter(|p| !p.is_empty());
            self.do_connect(ssid, pw.as_deref());
            self.set_expanded(state, root, None);
        }

        if let Some(c) = &mut self.composed {
            c.sync(state, root);
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
        let style = taffy::Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::auto(),
            },
            ..Default::default()
        };
        let node = taffy.new_with_children(style, &[child]).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node) {
                if let Some(&cn) = children.first() {
                    c.apply_layout(taffy, cn, Point::new(self.rect.x0, self.rect.y0));
                }
            }
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
        if let Some(c) = &mut self.composed {
            c.draw(scene, transform, text_renderer, theme, presence);
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
        if let Some(c) = &mut self.composed {
            if let Ok(children) = taffy.children(node_id) {
                if let Some(&cn) = children.first() {
                    c.handle_event(
                        event,
                        scale_factor,
                        modifiers,
                        state,
                        path,
                        handled,
                        focus_target,
                        cn,
                        taffy,
                    );
                }
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.composed
            .as_ref()
            .map(|c| c.as_ref() as &dyn Widget)
            .into_iter()
            .collect()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.composed
            .as_mut()
            .map(|c| c.as_mut() as &mut (dyn Widget + '_))
            .into_iter()
            .collect()
    }

    fn name(&self) -> &str {
        "NetworkPane"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Bubble the expansion-driven rebuild request (the inline password
    /// prompt opening/closing) plus any child's own request, so the host
    /// re-runs `build_layout` and the prompt actually appears.
    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        let child = self
            .composed
            .as_mut()
            .map(|c| c.take_structure_dirty())
            .unwrap_or(false);
        own || child
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatcher::testing::RecordingDispatcher;

    fn sample_networks() -> Vec<WifiNetworkInfo> {
        vec![
            WifiNetworkInfo {
                ssid: "Home_Network".to_string(),
                signal: 4,
                secure: true,
                connected: true,
            },
            WifiNetworkInfo {
                ssid: "Cafe_Guest".to_string(),
                signal: 3,
                secure: false,
                connected: false,
            },
            WifiNetworkInfo {
                ssid: "Neighbour_5G".to_string(),
                signal: 2,
                secure: true,
                connected: false,
            },
        ]
    }

    /// The signal indicator must use the Material Design Icons `wifi-strength`
    /// family (Private Use Area, `U+F09xx`) — the same family the sidebar's
    /// Network glyph uses and which the shipped fonts actually carry — never the
    /// Unicode block elements (`U+258x`), which fall back to a `.notdef` tofu
    /// box and convey no signal level.
    #[test]
    fn wifi_signal_glyph_uses_the_mdi_family_not_block_elements() {
        for signal in 0u8..=6 {
            let g = wifi_signal_glyph(signal);
            let cp = g.chars().next().unwrap() as u32;
            assert!(
                (0xF091F..=0xF092B).contains(&cp),
                "signal {signal} glyph U+{cp:04X} must be an MDI wifi-strength glyph"
            );
            // Never a Unicode block element (the old tofu bars).
            assert!(!(0x2580..=0x259F).contains(&cp), "block-element bars are tofu");
        }
        // Distinct levels for 1..=4 so strength is legible at a glance.
        let levels: Vec<&str> = (1u8..=4).map(wifi_signal_glyph).collect();
        let unique: std::collections::HashSet<&&str> = levels.iter().collect();
        assert_eq!(unique.len(), 4, "each bar level is a distinct glyph");
    }

    #[test]
    fn wifi_list_renders_each_returned_network() {
        let rec = Arc::new(RecordingDispatcher::default());
        let nets = sample_networks();
        let pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            nets.clone(),
        );
        assert_eq!(pane.networks().len(), 3);
        let by_ssid: Vec<&str> = pane.networks().iter().map(|n| n.ssid.as_str()).collect();
        assert_eq!(by_ssid, vec!["Home_Network", "Cafe_Guest", "Neighbour_5G"]);
        // Status line picks the connected network.
        assert_eq!(pane.connection_status_text(), "Connected to Home_Network");
    }

    #[test]
    fn wifi_toggle_invokes_set_wifi() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // User flips the toggle off.
        let _ = doc.put(&root, WIFI_ENABLED_KEY, automerge::ScalarValue::Boolean(false));
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["set_wifi(false)"]);
    }

    #[test]
    fn connect_button_invokes_connect_wifi_once() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(
            &root,
            connect_key("Cafe_Guest").as_str(),
            automerge::ScalarValue::Boolean(true),
        );
        pane.sync(&mut doc, &root);
        pane.sync(&mut doc, &root);
        assert_eq!(rec.calls(), vec!["connect_wifi(Cafe_Guest)"]);
    }

    #[test]
    fn refresh_button_replaces_network_list() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        // Swap the lister to a fresh set so we can observe the refresh.
        let fresh = vec![WifiNetworkInfo {
            ssid: "NewSpot".to_string(),
            signal: 4,
            secure: false,
            connected: false,
        }];
        pane.lister = {
            let f = fresh.clone();
            Arc::new(move || Ok(f.clone()))
        };
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, REFRESH_KEY, automerge::ScalarValue::Boolean(true));
        pane.sync(&mut doc, &root);
        assert_eq!(pane.networks(), fresh.as_slice());
    }

    /// A dispatcher that captures the password handed to `connect_wifi`, so
    /// the password-prompt test can assert it actually reached the backend
    /// (the shared `RecordingDispatcher` drops the password).
    #[derive(Default)]
    struct PwRecorder {
        calls: std::sync::Mutex<Vec<String>>,
    }
    impl Dispatcher for PwRecorder {
        fn set_brightness(&self, _p: f32) -> Result<(), String> { Ok(()) }
        fn set_volume(&self, _p: f32) -> Result<(), String> { Ok(()) }
        fn set_wifi(&self, _e: bool) -> Result<(), String> { Ok(()) }
        fn set_bluetooth(&self, _e: bool) -> Result<(), String> { Ok(()) }
        fn connect_wifi(&self, ssid: &str, password: Option<&str>) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("connect_wifi({ssid}, pw={})", password.unwrap_or("<none>")));
            Ok(())
        }
        fn set_power_profile(&self, _p: os_actions::PowerProfile) -> Result<(), String> { Ok(()) }
    }

    /// Clicking "Connect" on a SECURE network does not connect immediately —
    /// it expands the inline password prompt (mirrored into the document)
    /// and fires no dispatch yet.
    #[test]
    fn connect_on_secure_network_opens_password_prompt() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Press Connect on the secure "Neighbour_5G".
        let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
        assert_eq!(
            read_str(&doc, &root, EXPANDED_KEY).as_deref(),
            Some("Neighbour_5G"),
            "expansion is mirrored into the document so it survives rebuilds"
        );
        assert!(rec.calls().is_empty(), "no connect dispatched before Join");
        assert!(pane.take_structure_dirty(), "expanding requests a rebuild");
    }

    /// Pressing "Join" in the prompt connects WITH the typed password and
    /// collapses the prompt.
    #[test]
    fn join_connects_with_typed_password() {
        let rec = Arc::new(PwRecorder::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        // Open the prompt, type a password, press Join.
        let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, password_key("Neighbour_5G").as_str(), "hunter2");
        let _ = doc.put(&root, join_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(
            rec.calls.lock().unwrap().as_slice(),
            ["connect_wifi(Neighbour_5G, pw=hunter2)"]
        );
        assert_eq!(pane.expanded_ssid, None, "Join collapses the prompt");
        assert_eq!(read_str(&doc, &root, EXPANDED_KEY).as_deref(), Some(""));
    }

    /// Pressing Connect a second time on the same expanded network collapses
    /// the prompt (it acts as a toggle / Cancel).
    #[test]
    fn second_connect_press_collapses_prompt() {
        let rec = Arc::new(RecordingDispatcher::default());
        let mut pane = NetworkPane::with_dispatcher(
            rec.clone() as Arc<dyn Dispatcher + Send + Sync>,
            true,
            sample_networks(),
        );
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        pane.sync(&mut doc, &root);
        let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.expanded_ssid.as_deref(), Some("Neighbour_5G"));
        let _ = doc.put(&root, connect_key("Neighbour_5G").as_str(), true);
        pane.sync(&mut doc, &root);
        assert_eq!(pane.expanded_ssid, None);
    }

    #[test]
    fn wired_rows_reflect_connected_snapshot() {
        let pane = NetworkPane::new_demo(sample_networks()).with_wired(WiredInfo {
            connected: true,
            device: Some("eth0".into()),
            speed: Some("100 Mb/s".into()),
            ipv4: Some("10.0.0.7".into()),
        });
        // The Wired card body builds three rows when connected.
        assert_eq!(pane.wired_rows().len(), 3);
    }

    #[test]
    fn wired_rows_show_not_connected_when_down() {
        let pane = NetworkPane::new_demo(sample_networks());
        // Default WiredInfo is disconnected → status + caption only.
        assert_eq!(pane.wired_rows().len(), 2);
    }

    #[test]
    fn parse_vpn_connections_filters_to_vpn_types_and_marks_active() {
        let all = "Wired connection 1:ethernet\n\
                   Work VPN:vpn\n\
                   home-wg:wireguard\n\
                   MyWifi:wifi\n";
        let active = "Wired connection 1\nhome-wg\n";
        let vpns = parse_vpn_connections(all, active);
        assert_eq!(vpns.len(), 2, "only vpn + wireguard rows: {vpns:?}");
        let work = vpns.iter().find(|v| v.name == "Work VPN").unwrap();
        let wg = vpns.iter().find(|v| v.name == "home-wg").unwrap();
        assert!(!work.active, "Work VPN isn't in the active set");
        assert!(wg.active, "home-wg is up");
    }

    #[test]
    fn vpn_rows_reflect_configured_profiles() {
        let pane = NetworkPane::new_demo(sample_networks()).with_vpns(vec![
            VpnInfo { name: "Work VPN".into(), active: true },
            VpnInfo { name: "Home WG".into(), active: false },
        ]);
        assert_eq!(pane.vpns().len(), 2);
        // 2 status rows + 1 caption.
        assert_eq!(pane.vpn_rows().len(), 3);
    }

    #[test]
    fn vpn_rows_show_empty_state_when_none() {
        let pane = NetworkPane::new_demo(sample_networks());
        // Just the "no profiles" caption.
        assert_eq!(pane.vpn_rows().len(), 1);
    }
}
