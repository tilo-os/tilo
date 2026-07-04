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
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::config::{self, Backend, CommandPlan};
use crate::doc::VpnDoc;
use crate::model::{
    human_ago, human_bytes, sample_vault, ConnId, ConnKind, ConnStatus, Connection, Vault,
};
use crate::throughput::{self, ThroughputModel};

const HEADER_H: f64 = 72.0;
const FOOTER_H: f64 = 56.0;
const LIST_W: f64 = 440.0;
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

/// Which add-form field has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AddField {
    Name,
    Endpoint,
}

pub struct VpnApp {
    vault: Vault,
    rect: Rect,
    node: Option<NodeId>,
    seeded: bool,
    started: Instant,
    /// (download, upload) waveform generators, per connection.
    meters: HashMap<ConnId, (ThroughputModel, ThroughputModel)>,
    detail_tab: DetailTab,
    modal: Modal,
    add_kind: ConnKind,
    add_name: String,
    add_endpoint: String,
    add_focus: AddField,
    pending: Option<Pending>,
    /// Most recent action results, newest last (demo activity log).
    activity: Vec<String>,
    last_mouse: Point,
    hovered_row: Option<ConnId>,
    is_focused: bool,
}

impl Default for VpnApp {
    fn default() -> Self {
        Self::new()
    }
}

impl VpnApp {
    pub fn new() -> Self {
        VpnApp {
            vault: Vault::default(),
            rect: Rect::ZERO,
            node: None,
            seeded: false,
            started: Instant::now(),
            meters: HashMap::new(),
            detail_tab: DetailTab::Status,
            modal: Modal::None,
            add_kind: ConnKind::WireGuard,
            add_name: String::new(),
            add_endpoint: String::new(),
            add_focus: AddField::Name,
            pending: None,
            activity: vec![
                "vpn: ready".to_string(),
                "wg: interface wg-home up — handshake ok".to_string(),
            ],
            last_mouse: Point::ZERO,
            hovered_row: None,
            is_focused: false,
        }
    }

    fn now_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    fn ensure_meter(meters: &mut HashMap<ConnId, (ThroughputModel, ThroughputModel)>, c: &Connection) {
        meters.entry(c.id).or_insert_with(|| {
            let salt = id_salt(c.id);
            (ThroughputModel::new(salt), ThroughputModel::new(salt + 7.0))
        });
    }

    /// Pull the vault from the CRDT, seeding the demo fleet on first run.
    fn refresh(&mut self, doc: &mut AutoCommit) {
        if !self.seeded {
            if VpnDoc::is_empty(doc) {
                VpnDoc::write_vault(doc, &sample_vault());
            }
            self.seeded = true;
        }
        self.vault = VpnDoc::read_vault(doc);
        if self.vault.selected.is_none() {
            if let Some(first) = self.vault.connections.first().map(|c| c.id) {
                self.vault.selected = Some(first);
                VpnDoc::set_selected(doc, Some(first));
            }
        }
        let conns = self.vault.connections.clone();
        for c in &conns {
            Self::ensure_meter(&mut self.meters, c);
        }
    }

    // ---- mutations (all go through the CRDT) -------------------------

    fn select(&mut self, doc: &mut AutoCommit, id: ConnId) {
        self.vault.selected = Some(id);
        VpnDoc::set_selected(doc, Some(id));
    }

    fn select_offset(&mut self, doc: &mut AutoCommit, delta: i32) {
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
        let id = self.vault.connections[next].id;
        self.select(doc, id);
    }

    /// Begin connecting/disconnecting a connection → raises a confirm.
    fn request_toggle(&mut self, id: ConnId) {
        let Some(c) = self.vault.get(id) else { return };
        let backend = config::default_backend(c.kind);
        let backend = if backend == Backend::OpenVpnCli {
            backend
        } else {
            Backend::WgQuick
        };
        self.pending = Some(if c.is_connected() {
            Pending::Disconnect(id, config::disconnect_plan(c, backend))
        } else {
            Pending::Connect(id, config::connect_plan(c, backend))
        });
        self.modal = Modal::Confirm;
    }

    fn request_tor_global(&mut self) {
        let next = !self.vault.tor_global;
        self.pending = Some(Pending::TorGlobal(next, config::tor_plan(next)));
        self.modal = Modal::Confirm;
    }

    /// Apply the pending action (after the user confirms). The network
    /// command is attempted but the UI updates optimistically so the app
    /// stays responsive without elevated privileges.
    fn confirm_pending(&mut self, doc: &mut AutoCommit) {
        let Some(p) = self.pending.take() else { return };
        match p {
            Pending::Connect(id, plan) => {
                VpnDoc::set_status(doc, id, ConnStatus::Connected);
                self.run_and_log(&plan);
            }
            Pending::Disconnect(id, plan) => {
                VpnDoc::set_status(doc, id, ConnStatus::Disconnected);
                self.run_and_log(&plan);
            }
            Pending::TorGlobal(on, plan) => {
                VpnDoc::set_tor_global(doc, on);
                self.run_and_log(&plan);
            }
        }
        self.modal = Modal::None;
        self.refresh(doc);
    }

    fn run_and_log(&mut self, plan: &CommandPlan) {
        match config::execute(plan) {
            Ok(out) => self.activity.push(format!("$ {}  → ok {}", plan.display(), out)),
            Err(e) => self.activity.push(format!("$ {}  → {}", plan.display(), e)),
        }
        if self.activity.len() > 50 {
            let drop = self.activity.len() - 50;
            self.activity.drain(0..drop);
        }
    }

    fn toggle_conn_tor(&mut self, doc: &mut AutoCommit, id: ConnId) {
        let next = self.vault.get(id).map(|c| !c.tor).unwrap_or(true);
        VpnDoc::set_conn_tor(doc, id, next);
        self.refresh(doc);
    }

    fn open_add(&mut self, kind: ConnKind) {
        self.add_kind = kind;
        self.add_name.clear();
        self.add_endpoint.clear();
        self.add_focus = AddField::Name;
        self.modal = Modal::Add;
    }

    fn commit_add(&mut self, doc: &mut AutoCommit) {
        let name = if self.add_name.trim().is_empty() {
            format!("New {}", self.add_kind.long_name())
        } else {
            self.add_name.trim().to_string()
        };
        let mut c = Connection::new(&name, self.add_kind);
        c.endpoint = self.add_endpoint.trim().to_string();
        c.last_used = "Just added".to_string();
        c.address = "10.0.0.2/32".to_string();
        c.dns = "1.1.1.1".to_string();
        c.allowed_ips = "0.0.0.0/0".to_string();
        c.public_key = match self.add_kind {
            ConnKind::WireGuard => "(import .conf to populate key)".to_string(),
            ConnKind::OpenVpn => format!("CN={name}"),
        };
        let id = c.id;
        VpnDoc::write_connection(doc, &c);
        self.modal = Modal::None;
        self.refresh(doc);
        self.select(doc, id);
        self.activity.push(format!("added {} '{}'", self.add_kind.badge(), name));
    }

    // ---- layout ------------------------------------------------------

    fn layout(&self) -> LayoutRects {
        let r = self.rect;
        let header = Rect::new(r.x0, r.y0, r.x1, r.y0 + HEADER_H);
        let footer = Rect::new(r.x0, r.y1 - FOOTER_H, r.x1, r.y1);
        let body = Rect::new(r.x0, header.y1, r.x1, footer.y0);
        let list = Rect::new(body.x0, body.y0, body.x0 + LIST_W, body.y1);
        let detail = Rect::new(list.x1, body.y0, body.x1, body.y1);

        // Header controls (right aligned).
        let add_btn = Rect::new(header.x1 - 24.0 - 132.0, header.y0 + 18.0, header.x1 - 24.0, header.y0 + 18.0 + 36.0);
        let tor_pill = Rect::new(add_btn.x0 - 12.0 - 150.0, header.y0 + 18.0, add_btn.x0 - 12.0, header.y0 + 18.0 + 36.0);

        // Rows.
        let mut rows = Vec::new();
        let mut y = list.y0 + 44.0; // leave room for the list section title
        for c in &self.vault.connections {
            let row = Rect::new(list.x0 + 12.0, y, list.x1 - 12.0, y + ROW_H);
            let toggle = Rect::new(row.x1 - 16.0 - 46.0, row.center().y - 13.0, row.x1 - 16.0, row.center().y + 13.0);
            rows.push((c.id, row, toggle));
            y += ROW_H + ROW_GAP;
        }

        // Detail header + tabs + content.
        let dpad = 24.0;
        let dinner = Rect::new(detail.x0 + dpad, detail.y0 + dpad, detail.x1 - dpad, detail.y1 - dpad);
        let connect_btn = Rect::new(dinner.x1 - 150.0, dinner.y0 + 6.0, dinner.x1, dinner.y0 + 6.0 + 42.0);
        let tabs_y = dinner.y0 + 76.0;
        let mut tabs = Vec::new();
        let mut tx = dinner.x0;
        for t in DetailTab::ALL {
            let w = 92.0;
            tabs.push((t, Rect::new(tx, tabs_y, tx + w, tabs_y + 34.0)));
            tx += w + 4.0;
        }
        let content = Rect::new(dinner.x0, tabs_y + 34.0 + 16.0, dinner.x1, dinner.y1);

        // Per-tunnel tor pill (in the Status tab kv-table) — computed on draw,
        // recomputed here for hit-testing.
        let tor_row = kv_tor_rect(content);

        // Footer buttons.
        let fy0 = footer.y0 + 10.0;
        let fy1 = footer.y1 - 10.0;
        let add_conn = Rect::new(footer.x0 + 20.0, fy0, footer.x0 + 20.0 + 150.0, fy1);
        let imp_conf = Rect::new(add_conn.x1 + 10.0, fy0, add_conn.x1 + 10.0 + 130.0, fy1);
        let imp_ovpn = Rect::new(imp_conf.x1 + 10.0, fy0, imp_conf.x1 + 10.0 + 130.0, fy1);

        // Modal card (centered).
        let modal_card = {
            let w = 480.0;
            let h = 300.0;
            let cx = r.center().x;
            let cy = r.center().y;
            Rect::new(cx - w / 2.0, cy - h / 2.0, cx + w / 2.0, cy + h / 2.0)
        };

        LayoutRects {
            header,
            footer,
            list,
            detail,
            add_btn,
            tor_pill,
            rows,
            connect_btn,
            tabs,
            content,
            tor_row,
            add_conn,
            imp_conf,
            imp_ovpn,
            modal_card,
        }
    }
}

struct LayoutRects {
    header: Rect,
    footer: Rect,
    list: Rect,
    detail: Rect,
    add_btn: Rect,
    tor_pill: Rect,
    rows: Vec<(ConnId, Rect, Rect)>,
    connect_btn: Rect,
    tabs: Vec<(DetailTab, Rect)>,
    content: Rect,
    tor_row: Rect,
    add_conn: Rect,
    imp_conf: Rect,
    imp_ovpn: Rect,
    modal_card: Rect,
}

/// Position of the per-tunnel-Tor pill inside the Status kv table.
fn kv_tor_rect(content: Rect) -> Rect {
    // 5th kv row; rows are 30px tall starting after the chart.
    let kv_top = content.y0 + 72.0 + 16.0 + 176.0 + 16.0;
    let row_y = kv_top + 4.0 * 30.0;
    Rect::new(content.x1 - 84.0, row_y + 2.0, content.x1 - 84.0 + 64.0, row_y + 26.0)
}

fn id_salt(id: ConnId) -> f64 {
    let b = id.as_bytes();
    let mut acc: u64 = 0;
    for &x in b {
        acc = acc.wrapping_mul(31).wrapping_add(x as u64);
    }
    (acc % 1000) as f64 / 50.0
}

fn point_in(r: Rect, p: Point) -> bool {
    p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
}

// ----------------------------------------------------------------------
// Drawing primitives
// ----------------------------------------------------------------------

fn fill_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color) {
    scene.fill(Fill::NonZero, t, color, None, &RoundedRect::from_rect(r, radius));
}
fn stroke_round(scene: &mut Scene, t: Affine, r: Rect, radius: f64, color: Color, w: f64) {
    scene.stroke(&Stroke::new(w), t, color, None, &RoundedRect::from_rect(r, radius));
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
    tr.draw_text_weighted(scene, s, size, color, t * Affine::translate((x, top)), None, weight);
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
    let top = box_y0 + (box_h - size as f64) / 2.0;
    text_w(tr, scene, t, s, size, color, x, top, weight);
}

/// Center text horizontally + vertically within a rect (button labels).
fn text_center(tr: &mut TextRenderer, scene: &mut Scene, t: Affine, s: &str, size: f32, color: Color, r: Rect, weight: TextWeight) {
    let (w, _) = tr.measure_weighted(s, size, None, weight);
    let x = r.center().x - w as f64 / 2.0;
    let top = r.y0 + (r.height() - size as f64) / 2.0;
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

    fn draw_badge(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, x: f64, cy: f64, kind: ConnKind) -> f64 {
        let label = kind.badge();
        let (w, _) = tr.measure_weighted(label, 11.0, None, TextWeight::Bold);
        let pad = 8.0;
        let bw = w as f64 + pad * 2.0;
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

    fn draw_header(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects) {
        let h = lay.header;
        fill_round(scene, t, Rect::new(h.x0, h.y0, h.x1, h.y1), 0.0, theme.tokens.colors.surfaces.raised);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.tokens.colors.border_white_10,
            None,
            &Line::new(Point::new(h.x0, h.y1), Point::new(h.x1, h.y1)),
        );
        // Shield glyph mark.
        let mark = Rect::new(h.x0 + 24.0, h.y0 + 20.0, h.x0 + 24.0 + 32.0, h.y0 + 20.0 + 32.0);
        fill_round(scene, t, mark, 9.0, theme.tokens.colors.accent[5]);
        text_center(tr, scene, t, "V", 18.0, Color::WHITE, mark, TextWeight::Bold);

        text_w(tr, scene, t, "VPN", 20.0, theme.text_primary, h.x0 + 68.0, h.y0 + 14.0, TextWeight::Bold);
        let sub = format!(
            "{} saved · {} active",
            self.vault.connections.len(),
            self.vault.active_count()
        );
        text_w(tr, scene, t, &sub, 12.0, theme.text_dim, h.x0 + 68.0, h.y0 + 40.0, TextWeight::Regular);

        // Tor global pill.
        self.draw_pill_toggle(scene, tr, t, theme, lay.tor_pill, "Tor", self.vault.tor_global);

        // Add button (accent).
        let hov = point_in(lay.add_btn, self.last_mouse);
        let bg = if hov { theme.tokens.colors.accent[4] } else { theme.tokens.colors.accent[5] };
        fill_round(scene, t, lay.add_btn, theme.tokens.radii.md as f64, bg);
        text_center(tr, scene, t, "+ Add tunnel", 13.0, Color::WHITE, lay.add_btn, TextWeight::Semibold);
    }

    fn draw_pill_toggle(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, r: Rect, label: &str, on: bool) {
        let bg = if on {
            theme.tokens.colors.accent[5].multiply_alpha(0.22)
        } else {
            theme.tokens.colors.surfaces.base
        };
        fill_round(scene, t, r, theme.tokens.radii.full as f64, bg);
        stroke_round(
            scene,
            t,
            r,
            theme.tokens.radii.full as f64,
            if on { theme.tokens.colors.accent[5] } else { theme.tokens.colors.border_standard },
            1.0,
        );
        // status dot
        let dot: Color = if on {
            theme.tokens.colors.accent[6]
        } else {
            theme.tokens.colors.neutral[5]
        };
        scene.fill(Fill::NonZero, t, dot, None, &Circle::new(Point::new(r.x0 + 18.0, r.center().y), 5.0));
        let txt = format!("{label}  {}", if on { "On" } else { "Off" });
        text_vc(tr, scene, t, &txt, 12.0, theme.text_primary, r.x0 + 32.0, r.y0, r.height(), TextWeight::Semibold);
    }

    fn draw_list(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects) {
        let l = lay.list;
        // pane bg
        fill_round(scene, t, l, 0.0, theme.tokens.colors.surfaces.base);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.tokens.colors.border_white_10,
            None,
            &Line::new(Point::new(l.x1, l.y0), Point::new(l.x1, l.y1)),
        );
        text_w(tr, scene, t, "CONNECTIONS", 11.0, theme.text_dim, l.x0 + 16.0, l.y0 + 18.0, TextWeight::Bold);

        for (id, row, toggle) in &lay.rows {
            let c = match self.vault.get(*id) {
                Some(c) => c,
                None => continue,
            };
            let selected = self.vault.selected == Some(*id);
            let hovered = self.hovered_row == Some(*id);
            let radius = theme.tokens.radii.lg as f64;
            if selected {
                fill_round(scene, t, *row, radius, theme.tokens.colors.accent[5].multiply_alpha(0.16));
                stroke_round(scene, t, *row, radius, theme.tokens.colors.accent[5].multiply_alpha(0.7), 1.0);
            } else if hovered {
                fill_round(scene, t, *row, radius, theme.tokens.colors.surface_white_5);
            }

            // status dot
            let sc = Self::status_color(theme, c.status);
            scene.fill(Fill::NonZero, t, sc, None, &Circle::new(Point::new(row.x0 + 22.0, row.y0 + 24.0), 5.0));
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
            text_w(tr, scene, t, &name, 15.0, theme.text_primary, row.x0 + 38.0, row.y0 + 14.0, TextWeight::Semibold);
            let (nw, _) = tr.measure_weighted(&name, 15.0, None, TextWeight::Semibold);
            self.draw_badge(scene, tr, t, theme, row.x0 + 38.0 + nw as f64 + 10.0, row.y0 + 24.0, c.kind);

            // endpoint + last used
            let line2 = format!("{} · {}", ellipsize(&c.endpoint, 28), c.last_used);
            text_w(tr, scene, t, &line2, 12.0, theme.text_dim, row.x0 + 38.0, row.y0 + 44.0, TextWeight::Regular);

            // per-tunnel tor flag mark
            if c.tor {
                text_w(tr, scene, t, "tor", 10.0, theme.tokens.colors.accent[6], toggle.x0 - 36.0, row.y0 + 14.0, TextWeight::Bold);
            }

            // toggle switch
            self.draw_switch(scene, t, theme, *toggle, c.is_connected());
        }
    }

    fn draw_switch(&self, scene: &mut Scene, t: Affine, theme: &Theme, r: Rect, on: bool) {
        let track = if on { theme.tokens.colors.semantic.success } else { theme.tokens.colors.neutral[3] };
        fill_round(scene, t, r, theme.tokens.radii.full as f64, track);
        let kr = 9.0;
        let kx = if on { r.x1 - kr - 3.0 } else { r.x0 + kr + 3.0 };
        scene.fill(Fill::NonZero, t, Color::WHITE, None, &Circle::new(Point::new(kx, r.center().y), kr));
    }

    fn draw_detail(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects) {
        let d = lay.detail;
        fill_round(scene, t, d, 0.0, theme.tokens.colors.surfaces.base);

        let Some(c) = self.vault.selected_conn() else {
            text_w(tr, scene, t, "Select a connection", 16.0, theme.text_dim, d.x0 + 24.0, d.y0 + 24.0, TextWeight::Regular);
            return;
        };
        let dinner = Rect::new(d.x0 + 24.0, d.y0 + 24.0, d.x1 - 24.0, d.y1 - 24.0);

        // Title + status badge.
        text_w(tr, scene, t, &c.name, 24.0, theme.text_primary, dinner.x0, dinner.y0, TextWeight::Bold);
        let (tw, _) = tr.measure_weighted(&c.name, 24.0, None, TextWeight::Bold);
        // status pill
        let sc = Self::status_color(theme, c.status);
        let pill_x = dinner.x0 + tw as f64 + 14.0;
        let plabel = c.status.label();
        let (pw, _) = tr.measure_weighted(plabel, 12.0, None, TextWeight::Semibold);
        let pill = Rect::new(pill_x, dinner.y0 + 2.0, pill_x + pw as f64 + 28.0, dinner.y0 + 26.0);
        fill_round(scene, t, pill, theme.tokens.radii.full as f64, sc.multiply_alpha(0.18));
        scene.fill(Fill::NonZero, t, sc, None, &Circle::new(Point::new(pill.x0 + 12.0, pill.center().y), 4.0));
        text_vc(tr, scene, t, plabel, 12.0, sc, pill.x0 + 22.0, pill.y0, pill.height(), TextWeight::Semibold);

        let sub = format!("{} · {}", c.kind.long_name(), ellipsize(&c.endpoint, 40));
        text_w(tr, scene, t, &sub, 13.0, theme.text_dim, dinner.x0, dinner.y0 + 34.0, TextWeight::Regular);

        // Connect / Disconnect button.
        let hov = point_in(lay.connect_btn, self.last_mouse);
        let (bg, fg, label) = if c.is_connected() {
            (theme.tokens.colors.semantic.danger.multiply_alpha(if hov { 1.0 } else { 0.9 }), Color::WHITE, "Disconnect")
        } else {
            (if hov { theme.tokens.colors.accent[4] } else { theme.tokens.colors.accent[5] }, Color::WHITE, "Connect")
        };
        fill_round(scene, t, lay.connect_btn, theme.tokens.radii.md as f64, bg);
        text_center(tr, scene, t, label, 14.0, fg, lay.connect_btn, TextWeight::Semibold);

        // Tabs.
        for (tab, r) in &lay.tabs {
            let active = *tab == self.detail_tab;
            if active {
                fill_round(scene, t, *r, theme.tokens.radii.md as f64, theme.tokens.colors.surfaces.raised);
                stroke_round(scene, t, *r, theme.tokens.radii.md as f64, theme.tokens.colors.border_white_10, 1.0);
            }
            let col = if active { theme.text_primary } else { theme.text_dim };
            text_center(tr, scene, t, tab.label(), 13.0, col, *r, if active { TextWeight::Semibold } else { TextWeight::Regular });
        }

        match self.detail_tab {
            DetailTab::Status => self.draw_status_tab(scene, tr, t, theme, lay, c),
            DetailTab::Config => self.draw_config_tab(scene, tr, t, theme, lay, c),
            DetailTab::Logs => self.draw_logs_tab(scene, tr, t, theme, lay, c),
        }
    }

    fn draw_status_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects, c: &Connection) {
        let content = lay.content;
        let now = self.now_secs();
        let (down_m, up_m) = self.meters.get(&c.id).copied().unwrap_or((ThroughputModel::new(1.0), ThroughputModel::new(8.0)));
        let cur_down = if c.is_connected() { down_m.current(c.base_down_mbps, now) } else { 0.0 };
        let cur_up = if c.is_connected() { up_m.current(c.base_up_mbps, now) } else { 0.0 };

        // Stat tiles (4 across).
        let tiles_h = 72.0;
        let gap = 12.0;
        let tw = (content.width() - gap * 3.0) / 4.0;
        let tile_vals = [
            ("ASSIGNED IP", c.address.clone()),
            (
                "HANDSHAKE",
                match c.last_handshake_secs {
                    Some(s) if c.is_connected() => human_ago(s + (now as u64 % 5)),
                    _ => "—".to_string(),
                },
            ),
            ("DOWN", if c.is_connected() { throughput::rate_label(cur_down) } else { "—".to_string() }),
            ("UP", if c.is_connected() { throughput::rate_label(cur_up) } else { "—".to_string() }),
        ];
        for (i, (label, val)) in tile_vals.iter().enumerate() {
            let x = content.x0 + i as f64 * (tw + gap);
            let r = Rect::new(x, content.y0, x + tw, content.y0 + tiles_h);
            fill_round(scene, t, r, theme.tokens.radii.lg as f64, theme.tokens.colors.surfaces.raised);
            stroke_round(scene, t, r, theme.tokens.radii.lg as f64, theme.tokens.colors.border_white_10, 1.0);
            text_w(tr, scene, t, label, 10.0, theme.text_dim, r.x0 + 14.0, r.y0 + 14.0, TextWeight::Bold);
            let vcol = match *label {
                "DOWN" => theme.tokens.colors.semantic.info,
                "UP" => theme.tokens.colors.semantic.success,
                _ => theme.text_primary,
            };
            text_w(tr, scene, t, &ellipsize(val, 16), 17.0, vcol, r.x0 + 14.0, r.y0 + 36.0, TextWeight::Semibold);
        }

        // Throughput chart.
        let chart = Rect::new(content.x0, content.y0 + tiles_h + 16.0, content.x1, content.y0 + tiles_h + 16.0 + 176.0);
        let down = down_m.series(c.base_down_mbps, now);
        let up = up_m.series(c.base_up_mbps, now);
        throughput::draw_chart(scene, t, chart, theme, &down, &up, c.is_connected());
        // Chart title + legend.
        text_w(tr, scene, t, "Throughput", 12.0, theme.text_dim, chart.x0 + 12.0, chart.y0 + 10.0, TextWeight::Bold);
        let legend_y = chart.y0 + 10.0;
        let lx = chart.x1 - 220.0;
        scene.fill(Fill::NonZero, t, theme.tokens.colors.semantic.info, None, &Circle::new(Point::new(lx, legend_y + 6.0), 4.0));
        text_w(tr, scene, t, &format!("↓ {}", throughput::rate_label(cur_down)), 11.0, theme.text_primary, lx + 10.0, legend_y, TextWeight::Semibold);
        let lx2 = lx + 110.0;
        scene.fill(Fill::NonZero, t, theme.tokens.colors.semantic.success, None, &Circle::new(Point::new(lx2, legend_y + 6.0), 4.0));
        text_w(tr, scene, t, &format!("↑ {}", throughput::rate_label(cur_up)), 11.0, theme.text_primary, lx2 + 10.0, legend_y, TextWeight::Semibold);

        // Key/value table.
        let kv_top = chart.y1 + 16.0;
        let rows = [
            ("Endpoint", c.endpoint.clone()),
            ("DNS", c.dns.clone()),
            ("Allowed IPs", c.allowed_ips.clone()),
            ("Public key", ellipsize(&c.public_key, 40)),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let ry = kv_top + i as f64 * 30.0;
            text_w(tr, scene, t, k, 12.0, theme.text_dim, content.x0, ry + 6.0, TextWeight::Regular);
            text_w(tr, scene, t, v, 12.0, theme.text_primary, content.x0 + 130.0, ry + 6.0, TextWeight::Regular);
            scene.stroke(
                &Stroke::new(1.0),
                t,
                theme.tokens.colors.border_white_10.multiply_alpha(0.5),
                None,
                &Line::new(Point::new(content.x0, ry + 30.0), Point::new(content.x1, ry + 30.0)),
            );
        }
        // Per-tunnel Tor row (with a small toggle).
        let ry = kv_top + 4.0 * 30.0;
        text_w(tr, scene, t, "Route over Tor", 12.0, theme.text_dim, content.x0, ry + 6.0, TextWeight::Regular);
        self.draw_switch(scene, t, theme, lay.tor_row, c.tor);
    }

    fn draw_config_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects, c: &Connection) {
        let content = lay.content;
        let card = Rect::new(content.x0, content.y0, content.x1, content.y1.min(content.y0 + 360.0));
        fill_round(scene, t, card, theme.tokens.radii.lg as f64, theme.tokens.colors.neutral[0]);
        stroke_round(scene, t, card, theme.tokens.radii.lg as f64, theme.tokens.colors.border_white_10, 1.0);
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
            tr.draw_text_styled(scene, line, 12.5, col, t * Affine::translate((card.x0 + 16.0, y)), None, TextWeight::Regular, false, true);
            y += 19.0;
            if y > card.y1 - 16.0 {
                break;
            }
        }
    }

    fn draw_logs_tab(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects, c: &Connection) {
        let content = lay.content;
        let card = Rect::new(content.x0, content.y0, content.x1, content.y1.min(content.y0 + 360.0));
        fill_round(scene, t, card, theme.tokens.radii.lg as f64, theme.tokens.colors.neutral[0]);
        stroke_round(scene, t, card, theme.tokens.radii.lg as f64, theme.tokens.colors.border_white_10, 1.0);
        let lines = synth_logs(c, &self.activity);
        let mut y = card.y0 + 14.0;
        for line in &lines {
            let col = if line.contains("error") || line.contains("denied") || line.contains("disabled") {
                theme.tokens.colors.semantic.warning
            } else if line.contains("ok") || line.contains("up") || line.contains("handshake") {
                theme.tokens.colors.semantic.success
            } else {
                theme.text_dim
            };
            tr.draw_text_styled(scene, &ellipsize(line, 78), 12.0, col, t * Affine::translate((card.x0 + 16.0, y)), None, TextWeight::Regular, false, true);
            y += 19.0;
            if y > card.y1 - 16.0 {
                break;
            }
        }
    }

    fn draw_footer(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects) {
        let f = lay.footer;
        fill_round(scene, t, f, 0.0, theme.tokens.colors.surfaces.raised);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            theme.tokens.colors.border_white_10,
            None,
            &Line::new(Point::new(f.x0, f.y0), Point::new(f.x1, f.y0)),
        );
        for (r, label) in [
            (lay.add_conn, "+ Add connection"),
            (lay.imp_conf, "Import .conf"),
            (lay.imp_ovpn, "Import .ovpn"),
        ] {
            let hov = point_in(r, self.last_mouse);
            let bg = if hov { theme.tokens.colors.surface_white_10 } else { theme.tokens.colors.surfaces.base };
            fill_round(scene, t, r, theme.tokens.radii.md as f64, bg);
            stroke_round(scene, t, r, theme.tokens.radii.md as f64, theme.tokens.colors.border_standard, 1.0);
            text_center(tr, scene, t, label, 12.0, theme.text_primary, r, TextWeight::Medium);
        }
        let tip = "↑/↓ select · Enter connect · a add · i import · t Tor · 1/2/3 tabs";
        let (w, _) = tr.measure_weighted(tip, 11.0, None, TextWeight::Regular);
        text_vc(tr, scene, t, tip, 11.0, theme.text_dim, f.x1 - 20.0 - w as f64, f.y0, f.height(), TextWeight::Regular);
    }

    fn draw_modal(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, lay: &LayoutRects) {
        if self.modal == Modal::None {
            return;
        }
        // Scrim.
        scene.fill(Fill::NonZero, t, theme.tokens.colors.surfaces.overlay, None, &self.rect);
        let card = lay.modal_card;
        fill_round(scene, t, card, theme.tokens.radii.xl as f64, theme.tokens.colors.surfaces.raised);
        stroke_round(scene, t, card, theme.tokens.radii.xl as f64, theme.tokens.colors.border_white_20, 1.0);
        match self.modal {
            Modal::Add => self.draw_add_modal(scene, tr, t, theme, card),
            Modal::Confirm => self.draw_confirm_modal(scene, tr, t, theme, card),
            Modal::None => {}
        }
    }

    fn draw_add_modal(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, card: Rect) {
        let x = card.x0 + 28.0;
        text_w(tr, scene, t, "Add / import connection", 18.0, theme.text_primary, x, card.y0 + 24.0, TextWeight::Bold);
        text_w(tr, scene, t, "Protocol", 11.0, theme.text_dim, x, card.y0 + 60.0, TextWeight::Bold);
        // segmented kind selector
        let seg_y = card.y0 + 78.0;
        for (i, k) in [ConnKind::WireGuard, ConnKind::OpenVpn].into_iter().enumerate() {
            let sx = x + i as f64 * 130.0;
            let r = Rect::new(sx, seg_y, sx + 122.0, seg_y + 34.0);
            let active = self.add_kind == k;
            let bg = if active { theme.tokens.colors.accent[5].multiply_alpha(0.22) } else { theme.tokens.colors.surfaces.base };
            fill_round(scene, t, r, theme.tokens.radii.md as f64, bg);
            stroke_round(scene, t, r, theme.tokens.radii.md as f64, if active { theme.tokens.colors.accent[5] } else { theme.tokens.colors.border_standard }, 1.0);
            let lbl = format!("{} (.{})", k.long_name(), k.ext());
            text_center(tr, scene, t, &lbl, 12.0, theme.text_primary, r, if active { TextWeight::Semibold } else { TextWeight::Regular });
        }
        // fields
        self.draw_field(scene, tr, t, theme, "Name", &self.add_name, card.x0 + 28.0, card.y0 + 128.0, card.width() - 56.0, self.add_focus == AddField::Name);
        self.draw_field(scene, tr, t, theme, "Endpoint (host:port)", &self.add_endpoint, card.x0 + 28.0, card.y0 + 186.0, card.width() - 56.0, self.add_focus == AddField::Endpoint);
        // buttons
        let by = card.y1 - 52.0;
        let cancel = Rect::new(card.x1 - 28.0 - 200.0, by, card.x1 - 28.0 - 110.0, by + 36.0);
        let save = Rect::new(card.x1 - 28.0 - 100.0, by, card.x1 - 28.0, by + 36.0);
        fill_round(scene, t, cancel, theme.tokens.radii.md as f64, theme.tokens.colors.surfaces.base);
        stroke_round(scene, t, cancel, theme.tokens.radii.md as f64, theme.tokens.colors.border_standard, 1.0);
        text_center(tr, scene, t, "Cancel", 13.0, theme.text_primary, cancel, TextWeight::Medium);
        fill_round(scene, t, save, theme.tokens.radii.md as f64, theme.tokens.colors.accent[5]);
        text_center(tr, scene, t, "Save", 13.0, Color::WHITE, save, TextWeight::Semibold);
        text_w(tr, scene, t, "Tip: import a real .conf/.ovpn to auto-fill keys, DNS and routes.", 11.0, theme.text_dim, card.x0 + 28.0, by - 24.0, TextWeight::Regular);
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_field(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, label: &str, value: &str, x: f64, y: f64, w: f64, focused: bool) {
        text_w(tr, scene, t, label, 11.0, theme.text_dim, x, y, TextWeight::Bold);
        let r = Rect::new(x, y + 18.0, x + w, y + 18.0 + 34.0);
        fill_round(scene, t, r, theme.tokens.radii.md as f64, theme.tokens.colors.surfaces.base);
        stroke_round(scene, t, r, theme.tokens.radii.md as f64, if focused { theme.border_focus } else { theme.tokens.colors.border_standard }, if focused { 2.0 } else { 1.0 });
        let shown = if value.is_empty() { "" } else { value };
        let col = theme.text_primary;
        text_vc(tr, scene, t, shown, 13.0, col, r.x0 + 12.0, r.y0, r.height(), TextWeight::Regular);
        if focused {
            // caret
            let (vw, _) = tr.measure(shown, 13.0, None);
            let cx = r.x0 + 12.0 + vw as f64 + 1.0;
            scene.stroke(&Stroke::new(1.5), t, theme.border_focus, None, &Line::new(Point::new(cx, r.y0 + 8.0), Point::new(cx, r.y1 - 8.0)));
        }
    }

    fn draw_confirm_modal(&self, scene: &mut Scene, tr: &mut TextRenderer, t: Affine, theme: &Theme, card: Rect) {
        let x = card.x0 + 28.0;
        let (title, summary, cmd, danger) = match &self.pending {
            Some(Pending::Connect(_, p)) => ("Connect tunnel", p.summary.clone(), p.display(), false),
            Some(Pending::Disconnect(_, p)) => ("Disconnect tunnel", p.summary.clone(), p.display(), true),
            Some(Pending::TorGlobal(on, p)) => (
                if *on { "Enable Tor" } else { "Disable Tor" },
                p.summary.clone(),
                p.display(),
                false,
            ),
            None => ("Confirm", String::new(), String::new(), false),
        };
        text_w(tr, scene, t, title, 18.0, theme.text_primary, x, card.y0 + 28.0, TextWeight::Bold);
        text_w(tr, scene, t, &summary, 13.0, theme.text_dim, x, card.y0 + 64.0, TextWeight::Regular);
        // command preview box
        let cmd_box = Rect::new(x, card.y0 + 98.0, card.x1 - 28.0, card.y0 + 98.0 + 56.0);
        fill_round(scene, t, cmd_box, theme.tokens.radii.md as f64, theme.tokens.colors.neutral[0]);
        stroke_round(scene, t, cmd_box, theme.tokens.radii.md as f64, theme.tokens.colors.border_white_10, 1.0);
        text_w(tr, scene, t, "Will run:", 10.0, theme.text_dim, cmd_box.x0 + 12.0, cmd_box.y0 + 8.0, TextWeight::Bold);
        tr.draw_text_styled(scene, &ellipsize(&cmd, 52), 13.0, theme.tokens.colors.semantic.success, t * Affine::translate((cmd_box.x0 + 12.0, cmd_box.y0 + 26.0)), None, TextWeight::Regular, false, true);

        let by = card.y1 - 52.0;
        let cancel = Rect::new(card.x1 - 28.0 - 220.0, by, card.x1 - 28.0 - 120.0, by + 36.0);
        let ok = Rect::new(card.x1 - 28.0 - 110.0, by, card.x1 - 28.0, by + 36.0);
        fill_round(scene, t, cancel, theme.tokens.radii.md as f64, theme.tokens.colors.surfaces.base);
        stroke_round(scene, t, cancel, theme.tokens.radii.md as f64, theme.tokens.colors.border_standard, 1.0);
        text_center(tr, scene, t, "Cancel", 13.0, theme.text_primary, cancel, TextWeight::Medium);
        let okbg = if danger { theme.tokens.colors.semantic.danger } else { theme.tokens.colors.accent[5] };
        fill_round(scene, t, ok, theme.tokens.radii.md as f64, okbg);
        text_center(tr, scene, t, "Confirm", 13.0, Color::WHITE, ok, TextWeight::Semibold);
    }

    fn modal_button_rects(&self, lay: &LayoutRects) -> (Rect, Rect) {
        // (cancel, primary) — matches the draw layout above.
        let card = lay.modal_card;
        let by = card.y1 - 52.0;
        match self.modal {
            Modal::Add => (
                Rect::new(card.x1 - 28.0 - 200.0, by, card.x1 - 28.0 - 110.0, by + 36.0),
                Rect::new(card.x1 - 28.0 - 100.0, by, card.x1 - 28.0, by + 36.0),
            ),
            _ => (
                Rect::new(card.x1 - 28.0 - 220.0, by, card.x1 - 28.0 - 120.0, by + 36.0),
                Rect::new(card.x1 - 28.0 - 110.0, by, card.x1 - 28.0, by + 36.0),
            ),
        }
    }

    fn add_modal_hit(&mut self, doc: &mut AutoCommit, p: Point, lay: &LayoutRects) {
        let card = lay.modal_card;
        // kind segments
        let seg_y = card.y0 + 78.0;
        for (i, k) in [ConnKind::WireGuard, ConnKind::OpenVpn].into_iter().enumerate() {
            let sx = card.x0 + 28.0 + i as f64 * 130.0;
            let r = Rect::new(sx, seg_y, sx + 122.0, seg_y + 34.0);
            if point_in(r, p) {
                self.add_kind = k;
                return;
            }
        }
        // fields
        let name_r = Rect::new(card.x0 + 28.0, card.y0 + 146.0, card.x1 - 28.0, card.y0 + 180.0);
        let endp_r = Rect::new(card.x0 + 28.0, card.y0 + 204.0, card.x1 - 28.0, card.y0 + 238.0);
        if point_in(name_r, p) {
            self.add_focus = AddField::Name;
            return;
        }
        if point_in(endp_r, p) {
            self.add_focus = AddField::Endpoint;
            return;
        }
        let (cancel, save) = self.modal_button_rects(lay);
        if point_in(cancel, p) {
            self.modal = Modal::None;
        } else if point_in(save, p) {
            self.commit_add(doc);
        }
    }
}

fn render_config_text(c: &Connection) -> String {
    match c.kind {
        ConnKind::WireGuard => format!(
            "[Interface]\nPrivateKey = (hidden)\nAddress = {}\nDNS = {}\n\n[Peer]\nPublicKey = {}\nEndpoint = {}\nAllowedIPs = {}\nPersistentKeepalive = 25",
            c.address, c.dns, c.public_key, c.endpoint, c.allowed_ips
        ),
        ConnKind::OpenVpn => format!(
            "client\ndev tun\nproto udp\nremote {}\nresolv-retry infinite\nnobind\npersist-key\npersist-tun\nremote-cert-tls server\n# pushed routes: {}\n# dns: {}\n<ca>\n  (certificate)\n</ca>",
            c.endpoint.replace(':', " ").split(" (").next().unwrap_or(&c.endpoint),
            c.allowed_ips,
            c.dns
        ),
    }
}

fn synth_logs(c: &Connection, activity: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let iface = match c.kind {
        ConnKind::WireGuard => format!("wg-{}", c.name.to_lowercase().replace(' ', "")),
        ConnKind::OpenVpn => "tun0".to_string(),
    };
    if c.is_connected() {
        out.push(format!("[{}] handshake completed with {}", iface, c.endpoint));
        out.push(format!("[{}] interface up — assigned {}", iface, c.address));
        out.push(format!("[{}] route add {} dev {}", iface, c.allowed_ips, iface));
        out.push(format!("[{}] received {} this session", iface, human_bytes(c.rx_bytes)));
        out.push(format!("[{}] sent {} this session", iface, human_bytes(c.tx_bytes)));
        if c.tor {
            out.push(format!("[{}] traffic chained over Tor SOCKS 127.0.0.1:9050", iface));
        }
    } else {
        out.push(format!("[{}] interface down", iface));
        out.push(format!("[{}] last session ended ({})", iface, c.last_used));
    }
    // append global activity newest-last
    for a in activity.iter().rev().take(6) {
        out.push(a.clone());
    }
    out
}

impl Widget for VpnApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        self.refresh(state);
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        self.node = Some(node);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &PresenceCtx) {
        // Background.
        scene.fill(Fill::NonZero, transform, theme.tokens.colors.surfaces.base, None, &self.rect);
        let lay = self.layout();
        self.draw_list(scene, text_renderer, transform, theme, &lay);
        self.draw_detail(scene, text_renderer, transform, theme, &lay);
        self.draw_header(scene, text_renderer, transform, theme, &lay);
        self.draw_footer(scene, text_renderer, transform, theme, &lay);
        self.draw_modal(scene, text_renderer, transform, theme, &lay);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &Modifiers,
        state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
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
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                let lay = self.layout();
                *handled = true;
                // Modal first (captures everything).
                if self.modal == Modal::Add {
                    self.add_modal_hit(state, p, &lay);
                    return;
                }
                if self.modal == Modal::Confirm {
                    let (cancel, ok) = self.modal_button_rects(&lay);
                    if point_in(ok, p) {
                        self.confirm_pending(state);
                    } else if point_in(cancel, p) {
                        self.pending = None;
                        self.modal = Modal::None;
                    }
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
                // Footer.
                if point_in(lay.add_conn, p) {
                    self.open_add(ConnKind::WireGuard);
                    return;
                }
                if point_in(lay.imp_conf, p) {
                    self.open_add(ConnKind::WireGuard);
                    return;
                }
                if point_in(lay.imp_ovpn, p) {
                    self.open_add(ConnKind::OpenVpn);
                    return;
                }
                // Detail tabs + connect button + per-tunnel tor.
                for (tab, r) in &lay.tabs {
                    if point_in(*r, p) {
                        self.detail_tab = *tab;
                        return;
                    }
                }
                if point_in(lay.connect_btn, p) {
                    if let Some(id) = self.vault.selected {
                        self.request_toggle(id);
                    }
                    return;
                }
                if self.detail_tab == DetailTab::Status && point_in(lay.tor_row, p) {
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
                        self.select(state, *id);
                        return;
                    }
                }
            }
            WindowEvent::KeyboardInput { event: KeyEvent { logical_key, state: ElementState::Pressed, text, .. }, .. } => {
                // Modal key handling.
                if self.modal == Modal::Add {
                    *handled = true;
                    match logical_key {
                        Key::Named(NamedKey::Escape) => self.modal = Modal::None,
                        Key::Named(NamedKey::Enter) => self.commit_add(state),
                        Key::Named(NamedKey::Tab) => {
                            self.add_focus = match self.add_focus {
                                AddField::Name => AddField::Endpoint,
                                AddField::Endpoint => AddField::Name,
                            };
                        }
                        Key::Named(NamedKey::Backspace) => {
                            let f = match self.add_focus {
                                AddField::Name => &mut self.add_name,
                                AddField::Endpoint => &mut self.add_endpoint,
                            };
                            f.pop();
                        }
                        _ => {
                            if let Some(txt) = text {
                                let s = txt.as_str();
                                if !s.is_empty() && !s.chars().any(|c| c.is_control()) {
                                    let f = match self.add_focus {
                                        AddField::Name => &mut self.add_name,
                                        AddField::Endpoint => &mut self.add_endpoint,
                                    };
                                    f.push_str(s);
                                }
                            }
                        }
                    }
                    return;
                }
                if self.modal == Modal::Confirm {
                    *handled = true;
                    match logical_key {
                        Key::Named(NamedKey::Enter) => self.confirm_pending(state),
                        Key::Named(NamedKey::Escape) => {
                            self.pending = None;
                            self.modal = Modal::None;
                        }
                        Key::Character(c) if c.as_str() == "y" => self.confirm_pending(state),
                        Key::Character(c) if c.as_str() == "n" => {
                            self.pending = None;
                            self.modal = Modal::None;
                        }
                        _ => {}
                    }
                    return;
                }
                // Global shortcuts.
                match logical_key {
                    Key::Named(NamedKey::ArrowDown) => {
                        self.select_offset(state, 1);
                        *handled = true;
                    }
                    Key::Named(NamedKey::ArrowUp) => {
                        self.select_offset(state, -1);
                        *handled = true;
                    }
                    Key::Named(NamedKey::Enter) => {
                        if let Some(id) = self.vault.selected {
                            self.request_toggle(id);
                        }
                        *handled = true;
                    }
                    Key::Character(c) => {
                        match c.as_str() {
                            "a" => { self.open_add(ConnKind::WireGuard); *handled = true; }
                            "i" => { self.open_add(ConnKind::OpenVpn); *handled = true; }
                            "t" => { self.request_tor_global(); *handled = true; }
                            "1" => { self.detail_tab = DetailTab::Status; *handled = true; }
                            "2" => { self.detail_tab = DetailTab::Config; *handled = true; }
                            "3" => { self.detail_tab = DetailTab::Logs; *handled = true; }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "VpnApp"
    }

    fn test_id(&self) -> Option<&str> {
        Some("vpn_app")
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
        // Keep redrawing while a tunnel is connected so the throughput
        // sparkline animates in the live loop.
        self.vault.active_count() > 0
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_doc() -> (VpnApp, AutoCommit) {
        let mut app = VpnApp::new();
        let mut doc = AutoCommit::new();
        app.refresh(&mut doc);
        (app, doc)
    }

    #[test]
    fn seeds_and_reads_back_fleet() {
        let (app, _doc) = app_with_doc();
        assert_eq!(app.vault.connections.len(), 4);
        assert!(app.vault.selected.is_some());
        assert_eq!(app.vault.active_count(), 1);
    }

    #[test]
    fn select_offset_wraps() {
        let (mut app, mut doc) = app_with_doc();
        let first = app.vault.selected.unwrap();
        app.select_offset(&mut doc, -1);
        let last = app.vault.selected.unwrap();
        assert_ne!(first, last);
        app.select_offset(&mut doc, 1);
        assert_eq!(app.vault.selected.unwrap(), first);
    }

    #[test]
    fn confirm_connect_flips_status_in_doc() {
        let (mut app, mut doc) = app_with_doc();
        let work = app.vault.connections.iter().find(|c| c.name == "Work").unwrap().id;
        app.select(&mut doc, work);
        app.request_toggle(work);
        assert_eq!(app.modal, Modal::Confirm);
        app.confirm_pending(&mut doc);
        let back = VpnDoc::read_vault(&doc);
        assert!(back.connections.iter().find(|c| c.id == work).unwrap().is_connected());
    }

    #[test]
    fn add_modal_commits_new_connection() {
        let (mut app, mut doc) = app_with_doc();
        let before = app.vault.connections.len();
        app.open_add(ConnKind::OpenVpn);
        app.add_name = "Travel".into();
        app.add_endpoint = "edge.example:1194".into();
        app.commit_add(&mut doc);
        assert_eq!(app.vault.connections.len(), before + 1);
        assert!(app.vault.connections.iter().any(|c| c.name == "Travel"));
    }
}
