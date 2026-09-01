//! Root widget for the firewall manager.
//!
//! `FirewallApp` is a single hand-painted [`ui::Widget`]: it owns the whole
//! [`FirewallConfig`], computes its own absolute layout from the window
//! rect, paints every region directly into the Vello scene, and hit-tests
//! pointer/keyboard events against the laid-out rects. The only embedded
//! toolkit widgets are two [`TextInput`]s (the port + source fields in the
//! add-rule form), positioned via `apply_layout_external` and driven by
//! forwarded events.
//!
//! State lives in the shared Automerge document (`doc.rs`): every mutation
//! re-serialises the config into the `config` Text object, and `sync`
//! adopts remote edits, so two instances pointed at one collab socket edit
//! the same ruleset live.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    SegmentedFace, SidebarDrawer, TextInput, Toggle, ToggleFace, ToggleGroup, Widget,
};

use crate::actions::FirewallAction;
use crate::backend::{Backend, Mode};
use crate::doc::FirewallDoc;
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, ServiceCatalog, Target, Zone, ZoneKind,
};
use crate::paint;

// ---- geometry constants -------------------------------------------------
const HEADER_H: f64 = 60.0;
const RAIL_W: f64 = 214.0;
const SERVICES_W: f64 = 300.0;
const PAD: f64 = 16.0;
const CARD_R: f64 = 12.0;
const SUMMARY_H: f64 = 134.0;
/// Taller summary when the stat chips wrap onto a second row at narrow widths.
const SUMMARY_H_NARROW: f64 = 188.0;
const FORM_H: f64 = 154.0;
/// The add-rule form stacks its controls onto four lines when narrow.
const FORM_H_NARROW: f64 = 226.0;
const RULE_ROW_H: f64 = 40.0;
/// Two-line rule rows (target + action on top, proto · source beneath) when
/// the table is too narrow for five abreast columns.
const RULE_ROW_H_NARROW: f64 = 58.0;
const SVC_ROW_H: f64 = 46.0;
const CTRL_H: f64 = 34.0;
const TOGGLE_SIZE: f64 = 34.0;

/// Below this window width the zone rail overlays (a summoned drawer) instead
/// of docking, the add-rule form stacks its controls, and the rules table
/// switches to two-line rows.
const NARROW_BREAKPOINT: f64 = 720.0;
/// The per-zone services panel only earns its ~300px column at/above this
/// width; below it collapses so the rules table + form get the whole center.
const SERVICES_BREAKPOINT: f64 = 1000.0;

/// Which rules are shown in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Allow,
    Deny,
}

impl Filter {
    fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::Allow => "Allow",
            Filter::Deny => "Deny",
        }
    }
    const ALL: [Filter; 3] = [Filter::All, Filter::Allow, Filter::Deny];
}

/// Which text field in the add-rule form currently has keyboard focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusField {
    Port,
    Source,
}

/// Draft state for the add-rule form.
struct FormState {
    is_service: bool,
    service_idx: usize,
    proto: Proto,
    action: Action,
    port: TextInput,
    source: TextInput,
    focus: Option<FocusField>,
}

impl FormState {
    fn new() -> Self {
        // Seed a plausible draft so the form reads as "ready to submit"
        // both live and in the headless screenshot.
        let mut port = TextInput::local().with_initial_value("8080");
        port.set_font_size(15.0);
        let mut source = TextInput::local().with_initial_value("10.13.0.0/24");
        source.set_font_size(15.0);
        source.set_focus(true);
        Self {
            is_service: false,
            service_idx: 0,
            proto: Proto::Tcp,
            action: Action::Allow,
            port,
            source,
            focus: Some(FocusField::Source),
        }
    }

    fn current_service(&self) -> String {
        ServiceCatalog::ALL
            .get(self.service_idx)
            .map_or_else(|| "ssh".to_string(), |s| s.id.to_string())
    }

    fn matcher(&self) -> Matcher {
        if self.is_service {
            Matcher::Service(self.current_service())
        } else {
            let p = self.port.value().trim();
            let port = if p.is_empty() {
                "0".to_string()
            } else {
                p.to_string()
            };
            Matcher::Port {
                port,
                proto: self.proto,
            }
        }
    }
}

/// Computed per-frame layout. Rebuilt by [`FirewallApp::relayout`].
#[derive(Default, Clone)]
struct Layout {
    /// Window is below [`NARROW_BREAKPOINT`]: rail overlays, form + table stack.
    narrow: bool,
    /// Window is at/above [`SERVICES_BREAKPOINT`]: the services panel is shown.
    services_visible: bool,

    header: Rect,
    enable_pill: Rect,
    panic_btn: Rect,
    apply_btn: Rect,

    rail: Rect,
    rail_toggle: Rect,
    zone_rows: Vec<(ZoneKind, Rect)>,

    summary: Rect,
    set_active_btn: Rect,

    filter_seg: Vec<Rect>,
    table: Rect,
    rule_rows: Vec<RuleRow>,

    form: Rect,
    kind_seg: Vec<Rect>,
    service_sel: Rect,
    service_prev: Rect,
    service_next: Rect,
    port_field: Rect,
    proto_seg: Vec<Rect>,
    action_seg: Vec<Rect>,
    source_field: Rect,
    add_btn: Rect,

    services: Rect,
    service_rows: Vec<(usize, Rect, Rect)>, // (catalog idx, row rect, toggle rect)
}

#[derive(Clone)]
struct RuleRow {
    id: RuleId,
    rect: Rect,
    toggle: Rect,
    delete: Rect,
}

pub struct FirewallApp {
    config: FirewallConfig,
    selected_zone: ZoneKind,
    selected_rule: Option<RuleId>,
    filter: Filter,
    form: FormState,
    backend: Backend,
    status: String,

    /// The zone rail as the shared adaptive drawer: docked beside the content
    /// on wide windows, a summoned overlay drawer when narrow.
    rail: SidebarDrawer,
    /// `--shots` hook: force the overlay rail open so the narrow drawer state
    /// can be captured headlessly (`QOS_FIREWALL_RAIL_OPEN=1`).
    force_rail_open: bool,

    rect: Rect,
    node: Option<NodeId>,
    layout: Layout,
    last_cursor: Point,

    /// JSON last written to / read from the doc. Guards against re-adopting
    /// our own writes and lets `sync` detect genuine remote edits.
    last_json: Option<String>,
    seeded: bool,
    /// Whether the displayed config reflects a real, readable firewall.
    /// `false` means firewalld is absent/stopped and we show an honest
    /// unavailable state instead of any fabricated ruleset.
    available: bool,

    /// Framework-owned action dispatch: every on-screen affordance routes its
    /// [`FirewallAction`] through here so it brackets exactly **one** undo
    /// boundary, encodes under [`FIREWALL_ACTION_TYPE`](crate::actions::FIREWALL_ACTION_TYPE),
    /// and replays byte-identically on peers — the same path Ctrl+K takes.
    actions: ui::ActionHost,
}

impl Default for FirewallApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FirewallApp {
    /// The live app: read the **real** current firewall from the host. If
    /// firewalld is absent or stopped, start on an honest empty/unavailable
    /// state — never a fabricated ruleset.
    pub fn new() -> Self {
        match crate::backend::import() {
            Some(config) => Self::from_parts(config, true),
            None => Self::from_parts(FirewallConfig::empty(), false),
        }
    }

    /// The hermetic `--shots` / demo build: seeded with a realistic sample
    /// ruleset so the headless screenshot is fully populated offline. This
    /// is the ONLY place the sample config is used — the live app never
    /// fabricates a firewall.
    pub fn new_demo() -> Self {
        Self::from_parts(crate::sample::sample_config(), true)
    }

    fn from_parts(config: FirewallConfig, available: bool) -> Self {
        let selected_zone = config.default_zone;
        Self {
            config,
            selected_zone,
            selected_rule: None,
            filter: Filter::All,
            form: FormState::new(),
            backend: Backend::detect(),
            status: String::new(),
            rail: SidebarDrawer::new()
                .left()
                .docked_width(RAIL_W)
                .overlay_width(RAIL_W.max(264.0))
                .breakpoint(NARROW_BREAKPOINT),
            force_rail_open: std::env::var("QOS_FIREWALL_RAIL_OPEN").as_deref() == Ok("1"),
            rect: Rect::ZERO,
            node: None,
            layout: Layout::default(),
            last_cursor: Point::ZERO,
            last_json: None,
            seeded: false,
            available,
            actions: ui::ActionHost::new(),
        }
    }

    fn selected_zone_ref(&self) -> Option<&Zone> {
        self.config.zone(self.selected_zone)
    }

    /// Indices into the selected zone's `rules` that pass the active filter.
    fn filtered_indices(&self) -> Vec<usize> {
        let Some(z) = self.selected_zone_ref() else {
            return Vec::new();
        };
        z.rules
            .iter()
            .enumerate()
            .filter(|(_, r)| match self.filter {
                Filter::All => true,
                Filter::Allow => r.action == Action::Allow,
                Filter::Deny => r.action == Action::Deny,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Dispatch one [`FirewallAction`] — the single CRDT-mutation path shared
    /// with the Ctrl+K palette. The action is applied through the qOS action
    /// executor (a pinned actor + committed change, so it replays byte-identically
    /// on peers), exactly as the palette does. The document heads captured just
    /// before the first action of an event are stashed so the host records
    /// **one** undo boundary bracketing it ([`Widget::take_committed_action`]).
    /// The action mutates the shared config blob in the document; we then adopt
    /// the result back into the in-memory view so the next frame paints it, and
    /// remember the JSON so `sync` doesn't re-adopt our own write as a remote
    /// edit.
    ///
    /// This is *only* the CRDT mutation. Pushing the ruleset to the kernel
    /// (`firewall-cmd`/`nft`, see [`Self::apply`]) is a separate side effect
    /// driven by the resulting document state — never part of an action.
    fn perform_action(&mut self, state: &mut AutoCommit, action: &FirewallAction) {
        self.actions.dispatch_with_context(
            state,
            crate::actions::FIREWALL_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Firewall", "Firewall", action),
        );
        // Adopt the mutated config so the in-memory view matches the document.
        if let Some(json) = FirewallDoc::load_json(state) {
            if let Some(cfg) = FirewallConfig::from_json(&json) {
                self.config = cfg;
            }
            self.last_json = Some(json);
        }
    }

    /// The last action dispatched, for the `affordance == ActionSpec` covenant
    /// test.
    #[cfg(test)]
    fn last_dispatched(&self) -> Option<&qos_actions::EncodedAction> {
        self.actions.last_dispatched()
    }

    /// The configuration Ctrl+K builds its resource tree from, or `None` when the
    /// firewall is unavailable / has no zones (nothing to act on). Exposed to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`], which can't read the app's private fields directly.
    pub(crate) fn firewall_resource_ctx(&self) -> Option<FirewallConfig> {
        if !self.available || self.config.zones.is_empty() {
            None
        } else {
            Some(self.config.clone())
        }
    }

    // ---- layout ---------------------------------------------------------
    fn relayout(&mut self) {
        let r = self.rect;
        let mut l = Layout::default();
        if r.width() < 4.0 || r.height() < 4.0 {
            self.layout = l;
            return;
        }

        let narrow = r.width() < NARROW_BREAKPOINT;
        let services_visible = r.width() >= SERVICES_BREAKPOINT;
        l.narrow = narrow;
        l.services_visible = services_visible;

        let by = Self::layout_header(r, &mut l, narrow);
        let content = self.layout_rail(r, &mut l, by);
        self.layout_center(r, &mut l, content, narrow, services_visible);

        self.layout = l;
        // Keep embedded inputs positioned.
        self.form.port.apply_layout_external(self.layout.port_field);
        self.form
            .source
            .apply_layout_external(self.layout.source_field);
    }

    /// Header spans the full width; controls are right-aligned. Returns the
    /// header's vertical center, reused by [`Self::layout_rail`] for the
    /// hamburger toggle.
    fn layout_header(r: Rect, l: &mut Layout, narrow: bool) -> f64 {
        l.header = Rect::new(r.x0, r.y0, r.x1, r.y0 + HEADER_H);
        let by = r.y0 + HEADER_H / 2.0;
        let mut rx = r.x1 - PAD;
        let apply_w = if narrow { 76.0 } else { 92.0 };
        l.apply_btn = Rect::new(rx - apply_w, by - 17.0, rx, by + 17.0);
        rx = l.apply_btn.x0 - 8.0;
        let panic_w = if narrow { 78.0 } else { 104.0 };
        l.panic_btn = Rect::new(rx - panic_w, by - 17.0, rx, by + 17.0);
        rx = l.panic_btn.x0 - 8.0;
        let enable_w = if narrow { 80.0 } else { 92.0 };
        l.enable_pill = Rect::new(rx - enable_w, by - 17.0, rx, by + 17.0);
        by
    }

    /// Resolve the rail drawer (docked vs overlay), its toggle, and the zone
    /// rows within it. Returns the content rect the rail insets.
    fn layout_rail(&mut self, r: Rect, l: &mut Layout, by: f64) -> Rect {
        // Content area below the header. The rail drawer decides docked (wide)
        // vs overlay (narrow) from its width, insetting the content when docked.
        let content_outer = Rect::new(r.x0, r.y0 + HEADER_H, r.x1, r.y1);
        self.rail.set_docked_width(RAIL_W);
        self.rail.set_overlay_width(RAIL_W.max(264.0));
        let mut content = self.rail.resolve(content_outer);
        // `--shots`: force the overlay drawer open so its narrow state renders.
        if self.force_rail_open && self.rail.is_overlay() && !self.rail.is_open() {
            self.rail.open();
            self.rail.snap(true);
            content = self.rail.resolve(content_outer);
        }
        l.rail = self.rail.sidebar_rect();

        // Rail toggle: a docked-collapse chevron pinned inside the rail when
        // docked-open, otherwise a hamburger at the header's left edge.
        if self.rail.is_docked() && self.rail.is_open() {
            let rr = l.rail;
            l.rail_toggle = Rect::new(
                rr.x1 - 12.0 - TOGGLE_SIZE,
                rr.y0 + 10.0,
                rr.x1 - 12.0,
                rr.y0 + 10.0 + TOGGLE_SIZE,
            );
        } else {
            l.rail_toggle = Rect::new(
                l.header.x0 + 12.0,
                by - TOGGLE_SIZE / 2.0,
                l.header.x0 + 12.0 + TOGGLE_SIZE,
                by + TOGGLE_SIZE / 2.0,
            );
        }

        // Zone rows within the (possibly off-screen / sliding) rail rect.
        if l.rail.width() > 4.0 {
            let rows_top = l.rail.y0 + 44.0;
            let row_h = 32.0;
            for (i, kind) in ZoneKind::ALL.iter().enumerate() {
                let y = rows_top + i as f64 * row_h;
                let rect = Rect::new(l.rail.x0 + 8.0, y, l.rail.x1 - 8.0, y + row_h - 4.0);
                l.zone_rows.push((*kind, rect));
            }
        }

        content
    }

    /// Services panel, summary, filter row, form + table, and services rows —
    /// everything inside the rail-inset `content` rect.
    fn layout_center(
        &self,
        r: Rect,
        l: &mut Layout,
        content: Rect,
        narrow: bool,
        services_visible: bool,
    ) {
        // Services panel earns its column only when wide.
        if services_visible {
            l.services = Rect::new(content.x1 - SERVICES_W, content.y0, content.x1, content.y1);
        }
        let center_x1 = if services_visible {
            l.services.x0
        } else {
            content.x1
        };
        let center = Rect::new(content.x0, content.y0, center_x1, content.y1);

        // Center column inner box.
        let cx0 = center.x0 + PAD;
        let cx1 = center.x1 - PAD;
        let cy = center.y0 + PAD;
        let cy1 = r.y1 - PAD;

        let summary_h = if narrow { SUMMARY_H_NARROW } else { SUMMARY_H };
        l.summary = Rect::new(cx0, cy, cx1, cy + summary_h);
        // Set-active button inside summary, top-right.
        l.set_active_btn = Rect::new(
            l.summary.x1 - 12.0 - 118.0,
            l.summary.y0 + 12.0,
            l.summary.x1 - 12.0,
            l.summary.y0 + 12.0 + 30.0,
        );

        // Filter row.
        let fy = l.summary.y1 + PAD;
        let seg_w = if narrow { 60.0 } else { 72.0 };
        let seg_h = 30.0;
        let seg_total = seg_w * 3.0;
        for i in 0..3 {
            let x = cx1 - seg_total + f64::from(i) * seg_w;
            l.filter_seg.push(Rect::new(x, fy, x + seg_w, fy + seg_h));
        }

        // Form card pinned to the bottom (taller when its controls stack).
        let form_h = if narrow { FORM_H_NARROW } else { FORM_H };
        l.form = Rect::new(cx0, cy1 - form_h, cx1, cy1);

        // Rules table fills the gap.
        l.table = Rect::new(cx0, fy + seg_h + 10.0, cx1, l.form.y0 - PAD);
        self.build_table_rows(l, narrow);

        // Form internals.
        self.build_form(l, narrow);

        // Services rows.
        if services_visible {
            let svc_top = l.services.y0 + 48.0;
            for (i, def) in ServiceCatalog::ALL.iter().enumerate() {
                let y = svc_top + i as f64 * SVC_ROW_H;
                if y + SVC_ROW_H > l.services.y1 - 6.0 {
                    break;
                }
                let row = Rect::new(
                    l.services.x0 + 12.0,
                    y,
                    l.services.x1 - 12.0,
                    y + SVC_ROW_H - 6.0,
                );
                let toggle = Rect::new(
                    row.x1 - 44.0,
                    row.y0 + (row.height() - 24.0) / 2.0,
                    row.x1,
                    row.y0 + (row.height() - 24.0) / 2.0 + 24.0,
                );
                l.service_rows.push((i, row, toggle));
                let _ = def;
            }
        }
    }

    fn build_table_rows(&self, l: &mut Layout, narrow: bool) {
        let indices = self.filtered_indices();
        let Some(z) = self.selected_zone_ref() else {
            return;
        };
        let inner_x0 = l.table.x0 + 12.0;
        let inner_x1 = l.table.x1 - 12.0;
        let body_top = l.table.y0 + 34.0;
        let row_h = if narrow {
            RULE_ROW_H_NARROW
        } else {
            RULE_ROW_H
        };
        let max_rows = ((l.table.y1 - body_top) / row_h).floor().max(0.0) as usize;
        for (vis, &idx) in indices.iter().take(max_rows).enumerate() {
            // `indices` was just built from this same `z.rules` above.
            let Some(rule) = z.rules.get(idx) else {
                continue;
            };
            let y = body_top + vis as f64 * row_h;
            let rect = Rect::new(inner_x0, y, inner_x1, y + row_h - 4.0);
            let toggle = Rect::new(
                rect.x1 - 92.0,
                rect.y0 + (rect.height() - 22.0) / 2.0,
                rect.x1 - 92.0 + 38.0,
                rect.y0 + (rect.height() - 22.0) / 2.0 + 22.0,
            );
            let delete = Rect::new(
                rect.x1 - 30.0,
                rect.y0 + (rect.height() - 26.0) / 2.0,
                rect.x1,
                rect.y0 + (rect.height() - 26.0) / 2.0 + 26.0,
            );
            l.rule_rows.push(RuleRow {
                id: rule.id,
                rect,
                toggle,
                delete,
            });
        }
    }

    fn build_form(&self, l: &mut Layout, narrow: bool) {
        let fx0 = l.form.x0 + 16.0;
        let fx1 = l.form.x1 - 16.0;
        if narrow {
            self.build_form_narrow(l, fx0, fx1);
            return;
        }
        // Line 1.
        let y1 = l.form.y0 + 42.0;
        // kind segmented Service|Port
        let kind_w = 64.0;
        for i in 0..2 {
            let x = fx0 + f64::from(i) * kind_w;
            l.kind_seg.push(Rect::new(x, y1, x + kind_w, y1 + CTRL_H));
        }
        let mut x = fx0 + kind_w * 2.0 + 12.0;
        if self.form.is_service {
            // service cycle selector: [<] name [>]
            let sel_w = 188.0;
            l.service_prev = Rect::new(x, y1, x + 30.0, y1 + CTRL_H);
            l.service_sel = Rect::new(x + 30.0, y1, x + sel_w - 30.0, y1 + CTRL_H);
            l.service_next = Rect::new(x + sel_w - 30.0, y1, x + sel_w, y1 + CTRL_H);
            x += sel_w + 12.0;
        } else {
            // port field + proto segmented
            l.port_field = Rect::new(x, y1, x + 96.0, y1 + CTRL_H);
            x += 96.0 + 8.0;
            let pw = 52.0;
            for i in 0..2 {
                let px = x + f64::from(i) * pw;
                l.proto_seg.push(Rect::new(px, y1, px + pw, y1 + CTRL_H));
            }
            x += pw * 2.0 + 12.0;
        }
        let _ = x;
        // action segmented allow|deny — right aligned on line 1
        let act_w = 64.0;
        for i in 0..2 {
            let ax = fx1 - act_w * 2.0 + f64::from(i) * act_w;
            l.action_seg
                .push(Rect::new(ax, y1, ax + act_w, y1 + CTRL_H));
        }

        // Line 2: source field + Add button.
        let y2 = l.form.y0 + 42.0 + CTRL_H + 14.0;
        let add_w = 120.0;
        l.add_btn = Rect::new(fx1 - add_w, y2, fx1, y2 + CTRL_H);
        let src_x0 = fx0 + 64.0; // after "Source" label
        l.source_field = Rect::new(src_x0, y2, l.add_btn.x0 - 16.0, y2 + CTRL_H);
    }

    /// Stacked add-rule form for narrow windows: four short lines instead of a
    /// dense two-line row, so no control is crushed or clipped.
    fn build_form_narrow(&self, l: &mut Layout, fx0: f64, fx1: f64) {
        // Line 1: kind segmented (Service|Port) on the left, action on the right.
        let y1 = l.form.y0 + 40.0;
        let kind_w = 62.0;
        for i in 0..2 {
            let x = fx0 + f64::from(i) * kind_w;
            l.kind_seg.push(Rect::new(x, y1, x + kind_w, y1 + CTRL_H));
        }
        let act_w = 62.0;
        for i in 0..2 {
            let ax = fx1 - act_w * 2.0 + f64::from(i) * act_w;
            l.action_seg
                .push(Rect::new(ax, y1, ax + act_w, y1 + CTRL_H));
        }

        // Line 2: service selector OR port + proto, spanning the width.
        let y2 = y1 + CTRL_H + 12.0;
        if self.form.is_service {
            let prev_w = 34.0;
            l.service_prev = Rect::new(fx0, y2, fx0 + prev_w, y2 + CTRL_H);
            l.service_next = Rect::new(fx1 - prev_w, y2, fx1, y2 + CTRL_H);
            l.service_sel = Rect::new(fx0 + prev_w, y2, fx1 - prev_w, y2 + CTRL_H);
        } else {
            let pw = 52.0;
            let port_x1 = fx1 - pw * 2.0 - 8.0;
            l.port_field = Rect::new(fx0, y2, port_x1, y2 + CTRL_H);
            for i in 0..2 {
                let px = fx1 - pw * 2.0 + f64::from(i) * pw;
                l.proto_seg.push(Rect::new(px, y2, px + pw, y2 + CTRL_H));
            }
        }

        // Line 3: source field (label sits above it), full width.
        let y3 = y2 + CTRL_H + 26.0;
        l.source_field = Rect::new(fx0, y3, fx1, y3 + CTRL_H);

        // Line 4: Add button, full width.
        let y4 = y3 + CTRL_H + 12.0;
        l.add_btn = Rect::new(fx0, y4, fx1, y4 + CTRL_H);
    }

    // ---- mutations ------------------------------------------------------
    fn select_zone(&mut self, kind: ZoneKind) {
        self.selected_zone = kind;
        self.selected_rule = None;
    }

    fn cycle_zone(&mut self, delta: i32) {
        let cur = ZoneKind::ALL
            .iter()
            .position(|z| *z == self.selected_zone)
            .unwrap_or(0) as i32;
        let n = ZoneKind::ALL.len() as i32;
        let next = ((cur + delta) % n + n) % n;
        // `next` is always in `0..n` by construction of the double-modulo above.
        if let Some(&zone) = ZoneKind::ALL.get(next as usize) {
            self.select_zone(zone);
        }
    }

    fn set_form_focus(&mut self, f: Option<FocusField>) {
        self.form.focus = f;
        self.form.port.set_focus(f == Some(FocusField::Port));
        self.form.source.set_focus(f == Some(FocusField::Source));
    }

    fn add_rule_from_form(&mut self, state: &mut AutoCommit) {
        // Nothing to append to if the selected zone isn't in the config.
        if self.config.zone(self.selected_zone).is_none() {
            return;
        }
        let matcher = self.form.matcher();
        let source = self.form.source.value().trim().to_string();
        let action = self.form.action;
        let label = matcher.label();
        // Mint the rule (id and all) at authoring time, then dispatch it as the
        // action both the form and the palette build.
        let rule = Rule::new(matcher, action, source);
        let id = rule.id;
        self.perform_action(
            state,
            &crate::actions::add_rule_action(self.selected_zone, rule),
        );
        self.selected_rule = Some(id);
        self.status = format!("Added {} {} rule", action.label(), label);
    }

    fn delete_rule(&mut self, id: RuleId, state: &mut AutoCommit) {
        self.perform_action(
            state,
            &crate::actions::delete_rule_action(self.selected_zone, id),
        );
        if self.selected_rule == Some(id) {
            self.selected_rule = None;
        }
        self.status = "Rule deleted".to_string();
    }

    fn toggle_rule(&mut self, id: RuleId, state: &mut AutoCommit) {
        self.perform_action(
            state,
            &crate::actions::toggle_rule_action(self.selected_zone, id),
        );
    }

    fn toggle_service(&mut self, idx: usize, state: &mut AutoCommit) {
        let Some(def) = ServiceCatalog::ALL.get(idx) else {
            return;
        };
        let id = def.id.to_string();
        // Resolve the target state at authoring time so the action is
        // order-free on replay.
        let currently = self
            .config
            .zone(self.selected_zone)
            .is_some_and(|z| z.has_service(&id));
        let on = !currently;
        self.perform_action(
            state,
            &crate::actions::set_service_action(self.selected_zone, id, on),
        );
        self.status = format!(
            "{} {} in {}",
            def.name,
            if on { "enabled" } else { "disabled" },
            self.selected_zone.name()
        );
    }

    fn set_active_zone(&mut self, state: &mut AutoCommit) {
        self.perform_action(
            state,
            &crate::actions::set_default_zone_action(self.selected_zone),
        );
        self.status = format!("{} is now the active zone", self.selected_zone.name());
    }

    fn toggle_panic(&mut self, state: &mut AutoCommit) {
        let on = !self.config.panic_mode;
        self.perform_action(state, &crate::actions::set_panic_action(on));
        self.status = if on {
            "PANIC MODE ON — all traffic dropped".to_string()
        } else {
            "Panic mode off".to_string()
        };
    }

    fn toggle_enabled(&mut self, state: &mut AutoCommit) {
        let on = !self.config.enabled;
        self.perform_action(state, &crate::actions::set_enabled_action(on));
        self.status = if on {
            "Firewall enabled".to_string()
        } else {
            "Firewall stopped".to_string()
        };
    }

    fn apply(&mut self) {
        match self.backend.apply(&self.config) {
            Ok(()) => {
                let n = self.backend.last_plan().len();
                self.status = match self.backend.mode() {
                    Mode::Live => format!("Applied {n} firewall-cmd commands"),
                    Mode::DryRun => {
                        format!("Dry-run: {n} commands planned (set QOS_FIREWALL_APPLY=1)")
                    }
                };
            }
            Err(e) => self.status = format!("Apply failed: {e}"),
        }
    }

    // ---- event helpers --------------------------------------------------
    fn forward_to_input(
        input: &mut TextInput,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        let mut handled = false;
        let mut ft: Option<NodeId> = None;
        input.handle_event(
            event,
            scale_factor,
            &modifiers,
            state,
            path,
            &mut handled,
            &mut ft,
            NodeId::new(0),
            taffy,
        );
        handled
    }
}

fn seg_index(rects: &[Rect], p: Point) -> Option<usize> {
    rects.iter().position(|r| r.contains(p))
}

fn alpha(c: Color, a: u8) -> Color {
    Color::from_rgba8(c.r(), c.g(), c.b(), a)
}

impl Widget for FirewallApp {
    fn sync(&mut self, state: &mut AutoCommit, _path: &ObjId) {
        if !self.seeded {
            // Adopt an existing shared config, or publish the one we read
            // from the host. We only ever publish REAL data: a config we
            // imported from firewalld or (in the demo build) the sample.
            // An unavailable firewall has no zones, so nothing fabricated is
            // ever written into the shared document.
            if let Some(json) = FirewallDoc::load_json(state) {
                if let Some(cfg) = FirewallConfig::from_json(&json) {
                    self.config = cfg;
                    self.available = true;
                    if self.config.zone(self.selected_zone).is_none() {
                        self.selected_zone = self.config.default_zone;
                    }
                    self.last_json = Some(json);
                }
            } else if !self.config.zones.is_empty() {
                let json = FirewallDoc::store(state, &self.config);
                self.last_json = Some(json);
            }
            self.seeded = true;
            return;
        }
        // Adopt genuine remote edits (collab).
        if let Some(json) = FirewallDoc::load_json(state)
            && self.last_json.as_deref() != Some(json.as_str())
            && let Some(cfg) = FirewallConfig::from_json(&json)
        {
            self.config = cfg;
            if self.config.zone(self.selected_zone).is_none() {
                self.selected_zone = self.config.default_zone;
            }
            self.last_json = Some(json);
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
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
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        self.rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Advance the rail's eased reveal off a real wall-clock delta, then lay
        // out against the freshly-settled geometry so the drawer slides.
        self.rail.tick(false);
        self.relayout();
        self.draw_app(scene, t, tr, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        self.draw(scene, t, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor = Point::new(position.x / scale_factor, position.y / scale_factor);
                Self::forward_to_input(
                    &mut self.form.port,
                    event,
                    scale_factor,
                    *modifiers,
                    state,
                    path,
                    taffy,
                );
                Self::forward_to_input(
                    &mut self.form.source,
                    event,
                    scale_factor,
                    *modifiers,
                    state,
                    path,
                    taffy,
                );
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_key(event, *modifiers, state, path, handled, taffy);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_click(
                    self.last_cursor,
                    state,
                    handled,
                    scale_factor,
                    *modifiers,
                    path,
                    taffy,
                    event,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                Self::forward_to_input(
                    &mut self.form.port,
                    event,
                    scale_factor,
                    *modifiers,
                    state,
                    path,
                    taffy,
                );
                Self::forward_to_input(
                    &mut self.form.source,
                    event,
                    scale_factor,
                    *modifiers,
                    state,
                    path,
                    taffy,
                );
            }
            _ => {}
        }
        self.relayout();
    }

    fn name(&self) -> &'static str {
        "FirewallApp"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        self.rail.is_animating()
    }

    /// Publish the open configuration + its zones/rules so the host's Ctrl+K
    /// action palette scopes to them. Delegates to the
    /// [`ResourceProvider`](qos_actions::ResourceProvider) impl in
    /// [`crate::actions`].
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        <Self as qos_actions::ResourceProvider>::resources(self, doc)
    }

    /// Hand the host the heads captured before an on-screen affordance dispatched
    /// its firewall action, so it records one undo boundary for it (see
    /// [`Self::perform_action`]). Rising-edge: cleared on read.
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }
}

impl FirewallApp {
    fn handle_key(
        &mut self,
        event: &WindowEvent,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        taffy: &TaffyTree<()>,
    ) {
        let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
        else {
            return;
        };
        if key_event.state != ElementState::Pressed {
            return;
        }
        // Global keys first.
        match &key_event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.set_form_focus(None);
                *handled = true;
                return;
            }
            Key::Named(NamedKey::Tab) => {
                let next = match self.form.focus {
                    Some(FocusField::Source) if !self.form.is_service => Some(FocusField::Port),
                    _ => Some(FocusField::Source),
                };
                self.set_form_focus(next);
                *handled = true;
                return;
            }
            Key::Named(NamedKey::Enter) => {
                self.add_rule_from_form(state);
                *handled = true;
                return;
            }
            _ => {}
        }

        // If a text field is focused, route the original event to it.
        if let Some(f) = self.form.focus {
            let input = match f {
                FocusField::Port => &mut self.form.port,
                FocusField::Source => &mut self.form.source,
            };
            if Self::forward_to_input(input, event, 1.0, modifiers, state, path, taffy) {
                *handled = true;
            }
            return;
        }

        // No field focused: app-level shortcuts.
        match &key_event.logical_key {
            Key::Named(NamedKey::ArrowDown) => {
                self.cycle_zone(1);
                *handled = true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.cycle_zone(-1);
                *handled = true;
            }
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                if let Some(id) = self.selected_rule {
                    self.delete_rule(id, state);
                    *handled = true;
                }
            }
            Key::Character(c) => {
                let s = c.as_str();
                if let Ok(d) = s.parse::<usize>() {
                    if (1..=9).contains(&d)
                        && let Some(&zone) = ZoneKind::ALL.get(d - 1)
                    {
                        self.select_zone(zone);
                        *handled = true;
                    }
                } else if s.eq_ignore_ascii_case("p") {
                    self.toggle_panic(state);
                    *handled = true;
                } else if s.eq_ignore_ascii_case("a") {
                    self.set_form_focus(Some(FocusField::Source));
                    *handled = true;
                } else if s.eq_ignore_ascii_case("f") {
                    self.filter = match self.filter {
                        Filter::All => Filter::Allow,
                        Filter::Allow => Filter::Deny,
                        Filter::Deny => Filter::All,
                    };
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the click-dispatch entry point threading the shared event/input context; \
                  splitting the body into handle_click_* helpers (below) is what keeps this \
                  one under the line-length lint, not shrinking its argument list further"
    )]
    fn handle_click(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        handled: &mut bool,
        scale_factor: f64,
        modifiers: Modifiers,
        path: &ObjId,
        taffy: &TaffyTree<()>,
        event: &WindowEvent,
    ) {
        // Nothing is interactive while firewalld is unavailable — there is
        // no real config to edit or apply.
        if !self.available {
            return;
        }

        let l = self.layout.clone();

        if self.handle_click_rail(p, &l, handled) {
            return;
        }
        if self.handle_click_primary(p, state, &l, handled) {
            return;
        }
        if self.handle_click_form(p, state, &l, handled) {
            return;
        }
        if self.handle_click_text_field(
            p,
            event,
            scale_factor,
            modifiers,
            state,
            path,
            taffy,
            &l,
            handled,
        ) {
            return;
        }

        // Clicked empty space: drop field focus + rule selection.
        self.set_form_focus(None);
    }

    /// Rail drawer: a scrim tap dismisses an open overlay; the toggle flips
    /// it; a tap inside an open overlay drawer only selects a zone (and then
    /// closes it) — it never falls through to the content behind. Returns
    /// whether the click was consumed here.
    fn handle_click_rail(&mut self, p: Point, l: &Layout, handled: &mut bool) -> bool {
        if self.rail.press_dismisses(p) {
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.rail_toggle.contains(p) {
            self.rail.toggle();
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if self.rail.is_overlay() && self.rail.is_open() && self.rail.sidebar_contains(p) {
            for (kind, rect) in &l.zone_rows {
                if rect.contains(p) {
                    self.select_zone(*kind);
                    self.rail.close();
                    *handled = true;
                    self.set_form_focus(None);
                    return true;
                }
            }
            *handled = true;
            return true;
        }
        false
    }

    /// Header controls, docked zone rail, summary set-active, filter
    /// segmented, rule rows, and service toggles. Returns whether the click
    /// was consumed here.
    fn handle_click_primary(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        l: &Layout,
        handled: &mut bool,
    ) -> bool {
        // Header controls.
        if l.enable_pill.contains(p) {
            self.toggle_enabled(state);
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.panic_btn.contains(p) {
            self.toggle_panic(state);
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.apply_btn.contains(p) {
            self.apply();
            *handled = true;
            self.set_form_focus(None);
            return true;
        }

        // Zone rail.
        for (kind, rect) in &l.zone_rows {
            if rect.contains(p) {
                self.select_zone(*kind);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
        }

        // Summary set-active.
        if l.set_active_btn.contains(p) {
            self.set_active_zone(state);
            *handled = true;
            self.set_form_focus(None);
            return true;
        }

        // Filter segmented.
        if let Some(i) = seg_index(&l.filter_seg, p)
            && let Some(&filter) = Filter::ALL.get(i)
        {
            self.filter = filter;
            self.selected_rule = None;
            *handled = true;
            self.set_form_focus(None);
            return true;
        }

        // Rule rows: toggle / delete / select.
        for row in &l.rule_rows {
            if row.delete.contains(p) {
                self.delete_rule(row.id, state);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
            if row.toggle.contains(p) {
                self.toggle_rule(row.id, state);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
            if row.rect.contains(p) {
                self.selected_rule = Some(row.id);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
        }

        // Service toggles.
        for (idx, row, _toggle) in &l.service_rows {
            if row.contains(p) {
                self.toggle_service(*idx, state);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
        }

        false
    }

    /// Add-rule form controls: kind/proto/action segmented, service
    /// prev/next, add button. Returns whether the click was consumed here.
    fn handle_click_form(
        &mut self,
        p: Point,
        state: &mut AutoCommit,
        l: &Layout,
        handled: &mut bool,
    ) -> bool {
        if let Some(i) = seg_index(&l.kind_seg, p) {
            self.form.is_service = i == 0;
            self.set_form_focus(None);
            *handled = true;
            return true;
        }
        if l.service_prev.contains(p) {
            let n = ServiceCatalog::ALL.len();
            self.form.service_idx = (self.form.service_idx + n - 1) % n;
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.service_next.contains(p) || l.service_sel.contains(p) {
            let n = ServiceCatalog::ALL.len();
            self.form.service_idx = (self.form.service_idx + 1) % n;
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if let Some(i) = seg_index(&l.proto_seg, p)
            && let Some(&proto) = Proto::ALL.get(i)
        {
            self.form.proto = proto;
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if let Some(i) = seg_index(&l.action_seg, p)
            && let Some(&action) = Action::ALL.get(i)
        {
            self.form.action = action;
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.add_btn.contains(p) {
            self.add_rule_from_form(state);
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        false
    }

    /// Port/source text fields: focus + forward the press for caret
    /// placement. Returns whether the click was consumed here.
    #[allow(
        clippy::too_many_arguments,
        reason = "threads the same shared event/input context as handle_click, split out only \
                  to keep handle_click itself under the line-length lint"
    )]
    fn handle_click_text_field(
        &mut self,
        p: Point,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        taffy: &TaffyTree<()>,
        l: &Layout,
        handled: &mut bool,
    ) -> bool {
        if !self.form.is_service && l.port_field.contains(p) {
            self.set_form_focus(Some(FocusField::Port));
            Self::forward_to_input(
                &mut self.form.port,
                event,
                scale_factor,
                modifiers,
                state,
                path,
                taffy,
            );
            *handled = true;
            return true;
        }
        if l.source_field.contains(p) {
            self.set_form_focus(Some(FocusField::Source));
            Self::forward_to_input(
                &mut self.form.source,
                event,
                scale_factor,
                modifiers,
                state,
                path,
                taffy,
            );
            *handled = true;
            return true;
        }
        false
    }
}

// =========================================================================
// Rendering
// =========================================================================

/// Bucket a zone's coarse trust score to a semantic colour for the rail dot.
fn trust_color(theme: &Theme, trust: u8) -> Color {
    match trust {
        0..=1 => theme.tokens.colors.semantic.danger,
        2..=4 => theme.tokens.colors.semantic.warning,
        5..=6 => theme.tokens.colors.semantic.info,
        _ => theme.tokens.colors.semantic.success,
    }
}

fn draw_toggle(scene: &mut Scene, t: Affine, theme: &Theme, rect: Rect, on: bool) {
    Toggle::paint(scene, t, theme, &ToggleFace::new(rect, on));
}

impl FirewallApp {
    fn draw_segmented(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rects: &[Rect],
        labels: &[&str],
        selected: usize,
    ) {
        // Shared segmented control: contiguous rects render as one themed
        // strip (accent-selected + separators + border) via `ToggleGroup::paint`.
        ToggleGroup::paint(
            scene,
            t,
            tr,
            theme,
            &SegmentedFace::new(rects, labels, selected).radius(7.0),
        );
    }

    fn draw_app(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let l = self.layout.clone();
        // Whole-window background.
        paint::fill_rect(scene, t, self.rect, theme.tokens.colors.surfaces.base);

        self.draw_header(scene, t, tr, theme, &l);
        if !self.available {
            // Firewalld isn't readable — show an honest empty state rather
            // than any zones/rules, and never a fabricated ruleset.
            self.draw_unavailable(scene, t, tr, theme, &l);
            return;
        }
        // Center content first, so the (overlay) rail + scrim draw over it.
        self.draw_summary(scene, t, tr, theme, &l);
        self.draw_filter(scene, t, tr, theme, &l);
        self.draw_table(scene, t, tr, theme, &l);
        if l.services_visible {
            self.draw_services(scene, t, tr, theme, &l);
        }
        self.draw_form(scene, t, tr, theme, presence, &l);

        // Overlay drawer: dim the content behind the sliding rail.
        let scrim = self.rail.scrim_rect();
        if scrim.width() > 0.5 {
            self.rail.draw_scrim(scene, t);
        }
        // The zone rail (docked column, or the overlay drawer floating on top).
        if l.rail.width() > 4.0 {
            self.draw_rail(scene, t, tr, theme, &l);
        }
        // Collapse / summon affordance (themed pill from the shared primitive).
        let hov = l.rail_toggle.contains(self.last_cursor);
        self.rail.draw_toggle(scene, t, theme, l.rail_toggle, hov);
    }

    /// The honest "no firewall to show" state: firewalld is not installed
    /// or not running, so there is nothing real to display.
    fn draw_unavailable(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let area = Rect::new(self.rect.x0, l.header.y1, self.rect.x1, self.rect.y1);
        let cw = 520.0_f64.min(area.width() - 48.0);
        let ch = 150.0;
        let cx = area.x0 + (area.width() - cw) / 2.0;
        let cy = area.y0 + (area.height() - ch) / 2.0;
        let card = Rect::new(cx, cy, cx + cw, cy + ch);
        paint::fill_rrect(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);
        paint::stroke_rrect(scene, t, card, CARD_R, theme.border_standard, 1.0);
        paint::label(
            tr,
            scene,
            t,
            "firewalld unavailable",
            18.0,
            theme.text_primary,
            card.x0 + 24.0,
            card.y0 + 26.0,
            cw - 48.0,
        );
        paint::label(
            tr,
            scene,
            t,
            "The firewalld service is not installed or not running, so there",
            13.0,
            theme.text_dim,
            card.x0 + 24.0,
            card.y0 + 62.0,
            cw - 48.0,
        );
        paint::label(
            tr,
            scene,
            t,
            "is no live firewall to display. Start firewalld to manage zones.",
            13.0,
            theme.text_dim,
            card.x0 + 24.0,
            card.y0 + 84.0,
            cw - 48.0,
        );
        paint::label(
            tr,
            scene,
            t,
            "Run:  sudo systemctl enable --now firewalld",
            12.0,
            theme.text_dim,
            card.x0 + 24.0,
            card.y0 + 116.0,
            cw - 48.0,
        );
    }

    fn draw_header(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let h = l.header;
        paint::fill_rect(scene, t, h, theme.tokens.colors.neutral[0]);
        paint::hline(scene, t, h.x0, h.x1, h.y1, theme.border_standard, 1.0);

        self.draw_header_status(scene, t, tr, theme, l, h);

        // With no readable firewall there are no live controls to offer.
        if !self.available {
            return;
        }
        self.draw_header_controls(scene, t, tr, theme, l, h);
    }

    /// The contextual status line + its colour (panic vs. normal). Dropped at
    /// narrow widths, where there's no room for it beside the controls (the
    /// enable pill + red "Panic ON" button already carry active / panic
    /// state).
    fn draw_header_status(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
        h: Rect,
    ) {
        // The OS owns the window title/app-name — the header shows only the
        // contextual firewall status (functional, not an app wordmark).
        let sub = if !self.available {
            "firewalld not running — nothing to display".to_string()
        } else if self.config.panic_mode {
            "PANIC MODE — all traffic dropped".to_string()
        } else {
            format!(
                "firewalld · nftables · active zone: {}",
                self.config.default_zone.name()
            )
        };
        let sub_color = if self.config.panic_mode && self.available {
            theme.tokens.colors.semantic.danger
        } else {
            theme.text_primary
        };
        // When shown, shift it right past the header hamburger and clamp its
        // width under the controls.
        if !l.narrow {
            let toggle_in_header = !(self.rail.is_docked() && self.rail.is_open());
            let sub_x = if toggle_in_header {
                h.x0 + 12.0 + TOGGLE_SIZE + 12.0
            } else {
                h.x0 + 20.0
            };
            let ctrl_left = if self.available {
                l.enable_pill.x0 - 52.0
            } else {
                h.x1 - PAD
            };
            let sub_max = (ctrl_left - sub_x).max(80.0);
            paint::label(
                tr,
                scene,
                t,
                &sub,
                13.0,
                sub_color,
                sub_x,
                h.y0 + 18.0,
                sub_max,
            );
        }
    }

    /// Mode chip + enable pill + panic button + apply button, right-aligned
    /// in the header. Only reached once the firewall is available.
    #[allow(
        clippy::similar_names,
        reason = "bg/fg is the standard background/foreground pairing; renaming either would read worse"
    )]
    fn draw_header_controls(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
        h: Rect,
    ) {
        // Backend mode chip, just left of the enable pill (hidden when narrow).
        if !l.narrow {
            let mode_txt = match self.backend.mode() {
                Mode::Live => "live",
                Mode::DryRun => "dry-run",
            };
            paint::label_right_v(
                tr,
                scene,
                t,
                mode_txt,
                11.0,
                theme.text_dim,
                l.enable_pill.x0 - 10.0,
                h.y0,
                HEADER_H,
            );
        }

        // Enable pill.
        let (pill_bg, pill_fg, pill_txt) = if self.config.enabled {
            (
                alpha(theme.tokens.colors.semantic.success, 38),
                theme.tokens.colors.semantic.success,
                "● Active",
            )
        } else {
            (
                alpha(theme.tokens.colors.semantic.danger, 38),
                theme.tokens.colors.semantic.danger,
                "○ Stopped",
            )
        };
        paint::fill_rrect(scene, t, l.enable_pill, 16.0, pill_bg);
        let w = paint::text_width(tr, pill_txt, 12.0);
        paint::label_v(
            tr,
            scene,
            t,
            pill_txt,
            12.0,
            pill_fg,
            l.enable_pill.x0 + (l.enable_pill.width() - w) / 2.0,
            l.enable_pill.y0,
            l.enable_pill.height(),
            w + 2.0,
        );

        // Panic button.
        let (pb_bg, pb_fg) = if self.config.panic_mode {
            (theme.tokens.colors.semantic.danger, Color::WHITE)
        } else {
            (theme.tokens.colors.neutral[2], theme.text_dim)
        };
        paint::fill_rrect(scene, t, l.panic_btn, 16.0, pb_bg);
        let pt = if self.config.panic_mode {
            "Panic ON"
        } else {
            "Panic"
        };
        let w = paint::text_width(tr, pt, 12.0);
        paint::label_v(
            tr,
            scene,
            t,
            pt,
            12.0,
            pb_fg,
            l.panic_btn.x0 + (l.panic_btn.width() - w) / 2.0,
            l.panic_btn.y0,
            l.panic_btn.height(),
            w + 2.0,
        );

        // Apply button.
        paint::fill_rrect(scene, t, l.apply_btn, 16.0, theme.accent_primary);
        let at = "Apply";
        let w = paint::text_width(tr, at, 13.0);
        paint::label_v(
            tr,
            scene,
            t,
            at,
            13.0,
            Color::WHITE,
            l.apply_btn.x0 + (l.apply_btn.width() - w) / 2.0,
            l.apply_btn.y0,
            l.apply_btn.height(),
            w + 2.0,
        );
    }

    fn draw_rail(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let rail = l.rail;
        // Clip every rail pixel to the (possibly sliding) panel so an overlay
        // reveal never spills zone rows over the content beside it.
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            1.0,
            t,
            &rail,
        );
        // Opaque panel fill: transparent when docked (window bg matches), but a
        // real surface when the drawer floats over content in overlay mode.
        if self.rail.is_overlay() {
            paint::fill_rect(scene, t, rail, theme.tokens.colors.neutral[0]);
        }
        // Right divider.
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &vello::kurbo::Line::new(Point::new(rail.x1, rail.y0), Point::new(rail.x1, rail.y1)),
        );
        paint::label(
            tr,
            scene,
            t,
            "ZONES",
            11.0,
            theme.text_dim,
            rail.x0 + 18.0,
            rail.y0 + 18.0,
            120.0,
        );

        for (kind, rect) in &l.zone_rows {
            let selected = *kind == self.selected_zone;
            let is_default = *kind == self.config.default_zone;
            if selected {
                paint::fill_rrect(scene, t, *rect, 8.0, theme.tokens.colors.surface_white_10);
                // Accent left bar.
                let bar = Rect::new(rect.x0, rect.y0 + 5.0, rect.x0 + 3.0, rect.y1 - 5.0);
                paint::fill_rrect(scene, t, bar, 2.0, theme.accent_primary);
            }
            // Trust dot.
            let cy = f64::midpoint(rect.y0, rect.y1);
            paint::dot(
                scene,
                t,
                Point::new(rect.x0 + 16.0, cy),
                4.0,
                trust_color(theme, kind.trust()),
            );
            // Name.
            let name_color = if selected {
                theme.text_primary
            } else {
                theme.tokens.colors.neutral[7]
            };
            paint::label_v(
                tr,
                scene,
                t,
                kind.name(),
                14.0,
                name_color,
                rect.x0 + 30.0,
                rect.y0,
                rect.height(),
                120.0,
            );

            // Active marker chip.
            if is_default {
                let _ = paint::chip(
                    tr,
                    scene,
                    t,
                    "active",
                    9.0,
                    Color::WHITE,
                    theme.accent_primary,
                    rect.x1 - 52.0,
                    cy,
                    6.0,
                    16.0,
                    8.0,
                );
            } else {
                // Rule + service count.
                if let Some(z) = self.config.zone(*kind) {
                    let n = z.rules.len() + z.services.len();
                    if n > 0 {
                        paint::label_right_v(
                            tr,
                            scene,
                            t,
                            &n.to_string(),
                            11.0,
                            theme.text_dim,
                            rect.x1 - 8.0,
                            rect.y0,
                            rect.height(),
                        );
                    }
                }
            }
        }
        scene.pop_layer();
    }

    fn draw_summary(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let card = l.summary;
        let border = if self.config.panic_mode {
            theme.tokens.colors.semantic.danger
        } else {
            theme.border_standard
        };
        paint::fill_rrect(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);
        paint::stroke_rrect(scene, t, card, CARD_R, border, 1.0);

        let Some(z) = self.config.zone(self.selected_zone) else {
            return;
        };
        let inset_x = card.x0 + 18.0;
        self.draw_summary_title(scene, t, tr, theme, card, inset_x);
        Self::draw_summary_chips(scene, t, tr, theme, card, inset_x, z);
        self.draw_summary_set_active(scene, t, tr, theme, l);
    }

    /// Zone title + active-zone chip + description, left-aligned at `inset_x`.
    fn draw_summary_title(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        inset_x: f64,
    ) {
        paint::label(
            tr,
            scene,
            t,
            self.selected_zone.name(),
            22.0,
            theme.text_primary,
            inset_x,
            card.y0 + 14.0,
            260.0,
        );
        let title_w = paint::text_width(tr, self.selected_zone.name(), 22.0);
        if self.selected_zone == self.config.default_zone {
            let _ = paint::chip(
                tr,
                scene,
                t,
                "ACTIVE ZONE",
                9.0,
                Color::WHITE,
                theme.accent_primary,
                inset_x + title_w + 12.0,
                card.y0 + 26.0,
                7.0,
                16.0,
                8.0,
            );
        }
        paint::label(
            tr,
            scene,
            t,
            self.selected_zone.description(),
            12.0,
            theme.text_dim,
            inset_x,
            card.y0 + 46.0,
            card.width() - 36.0,
        );
    }

    /// Stat chips row: laid out left-to-right, wrapping onto further rows so
    /// a narrow card never runs them off its right edge.
    fn draw_summary_chips(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        card: Rect,
        inset_x: f64,
        z: &Zone,
    ) {
        let (allow, deny) = z.rule_counts();
        let ifaces = if z.interfaces.is_empty() {
            "—".to_string()
        } else {
            z.interfaces.join(", ")
        };
        let mut chips: Vec<(&str, String, Color)> = vec![
            (
                "target",
                z.target.short().to_string(),
                target_color(theme, z.target),
            ),
            ("services", z.services.len().to_string(), theme.text_primary),
            (
                "allow rules",
                allow.to_string(),
                theme.tokens.colors.semantic.success,
            ),
            (
                "deny rules",
                deny.to_string(),
                theme.tokens.colors.semantic.danger,
            ),
            ("interfaces", ifaces, theme.text_primary),
        ];
        if z.masquerade {
            chips.push((
                "nat",
                "masquerade".to_string(),
                theme.tokens.colors.semantic.info,
            ));
        }
        let gap = 10.0;
        let row_h = 48.0;
        let chips_top = card.y0 + 74.0;
        let max_x = card.x1 - 16.0;
        let mut sx = inset_x;
        let mut row = 0.0_f64;
        for (caption, value, color) in &chips {
            let w = Self::stat_chip_width(tr, caption, value);
            if sx + w > max_x && sx > inset_x + 0.5 {
                row += 1.0;
                sx = inset_x;
            }
            let cy = chips_top + row * row_h + 20.0;
            Self::stat_chip(scene, t, tr, theme, caption, value, *color, sx, cy);
            sx += w + gap;
        }
    }

    /// Set-active button, drawn only when the selected zone isn't already
    /// active.
    fn draw_summary_set_active(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        if self.selected_zone == self.config.default_zone {
            return;
        }
        paint::fill_rrect(
            scene,
            t,
            l.set_active_btn,
            8.0,
            theme.tokens.colors.neutral[2],
        );
        paint::stroke_rrect(scene, t, l.set_active_btn, 8.0, theme.accent_primary, 1.0);
        let txt = "Set as active";
        let w = paint::text_width(tr, txt, 12.0);
        paint::label_v(
            tr,
            scene,
            t,
            txt,
            12.0,
            theme.text_primary,
            l.set_active_btn.x0 + (l.set_active_btn.width() - w) / 2.0,
            l.set_active_btn.y0,
            l.set_active_btn.height(),
            w + 2.0,
        );
    }

    /// The width [`Self::stat_chip`] will paint for `caption`/`value`, so the
    /// summary can wrap chips before drawing them.
    fn stat_chip_width(tr: &mut TextRenderer, caption: &str, value: &str) -> f64 {
        let cap_w = paint::text_width(tr, caption, 9.0);
        let val_w = paint::text_width(tr, value, 13.0);
        cap_w.max(val_w) + 20.0
    }

    /// A two-tier stat: a dim caption with a coloured value beneath, framed
    /// in a soft chip. Returns the chip width.
    #[allow(
        clippy::too_many_arguments,
        reason = "a single-call-site draw primitive; bundling position/color/text into a \
                  struct would only add construction boilerplate for no real readability gain"
    )]
    fn stat_chip(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        caption: &str,
        value: &str,
        value_color: Color,
        x: f64,
        y_center: f64,
    ) -> f64 {
        let cap_w = paint::text_width(tr, caption, 9.0);
        let val_w = paint::text_width(tr, value, 13.0);
        let inner = cap_w.max(val_w);
        let w = inner + 20.0;
        let h = 40.0;
        let rect = Rect::new(x, y_center - h / 2.0, x + w, y_center + h / 2.0);
        paint::fill_rrect(scene, t, rect, 8.0, theme.tokens.colors.neutral[1]);
        paint::label(
            tr,
            scene,
            t,
            caption,
            9.0,
            theme.text_dim,
            x + 10.0,
            rect.y0 + 6.0,
            inner + 2.0,
        );
        paint::label(
            tr,
            scene,
            t,
            value,
            13.0,
            value_color,
            x + 10.0,
            rect.y0 + 19.0,
            inner + 2.0,
        );
        w
    }

    fn draw_filter(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let Some(first_seg) = l.filter_seg.first() else {
            return;
        };
        let y0 = first_seg.y0;
        paint::label_v(
            tr,
            scene,
            t,
            "Rules",
            15.0,
            theme.text_primary,
            l.summary.x0,
            y0,
            first_seg.height(),
            120.0,
        );
        let labels = [
            Filter::All.label(),
            Filter::Allow.label(),
            Filter::Deny.label(),
        ];
        let sel = Filter::ALL
            .iter()
            .position(|f| *f == self.filter)
            .unwrap_or(0);
        Self::draw_segmented(scene, t, tr, theme, &l.filter_seg, &labels, sel);
    }
}

/// A rule row's derived presentation: colors and the action chip's text,
/// computed once per row and shared between the narrow and wide layouts.
struct RowStyle {
    primary: Color,
    secondary: Color,
    dot_color: Color,
    action_color: Color,
    action_text: &'static str,
    dim: bool,
}

impl RowStyle {
    fn for_rule(theme: &Theme, rule: &Rule) -> Self {
        let dim = !rule.enabled;
        let primary = if dim {
            theme.text_dim
        } else {
            theme.text_primary
        };
        let secondary = if dim {
            alpha(theme.text_dim, 140)
        } else {
            theme.text_dim
        };
        let dot_color = if rule.matcher.is_service() {
            theme.accent_primary
        } else {
            theme.tokens.colors.semantic.info
        };
        let (action_color, action_text) = match rule.action {
            Action::Allow => (theme.tokens.colors.semantic.success, "allow"),
            Action::Deny => (theme.tokens.colors.semantic.danger, "deny"),
        };
        Self {
            primary,
            secondary,
            dot_color,
            action_color,
            action_text,
            dim,
        }
    }
}

impl FirewallApp {
    fn draw_table(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let table = l.table;
        paint::fill_rrect(scene, t, table, CARD_R, theme.tokens.colors.surfaces.raised);
        paint::stroke_rrect(scene, t, table, CARD_R, theme.border_standard, 1.0);

        let cols = rule_columns(table);
        Self::draw_table_header(scene, t, tr, theme, l, table, cols);

        if l.rule_rows.is_empty() {
            paint::label(
                tr,
                scene,
                t,
                "No rules match this filter.",
                13.0,
                theme.text_dim,
                cols.0,
                table.y0 + 44.0,
                300.0,
            );
            return;
        }

        let Some(z) = self.config.zone(self.selected_zone) else {
            return;
        };
        for row in &l.rule_rows {
            let Some(rule) = z.rules.iter().find(|r| r.id == row.id) else {
                continue;
            };
            self.draw_table_row(scene, t, tr, theme, l, cols, row, rule);
        }
    }

    /// Column headers — five columns collapse to two labels when narrow —
    /// and the separator line beneath them.
    fn draw_table_header(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
        table: Rect,
        cols: (f64, f64, f64, f64, f64),
    ) {
        let (cx_target, cx_proto, cx_action, cx_source, cx_ctrl) = cols;
        let hy = table.y0 + 8.0;
        let hc = theme.text_dim;
        if l.narrow {
            paint::label(tr, scene, t, "RULE", 10.0, hc, cx_target, hy, 200.0);
            paint::label(tr, scene, t, "ON", 10.0, hc, cx_ctrl, hy, 60.0);
        } else {
            paint::label(tr, scene, t, "TARGET", 10.0, hc, cx_target, hy, 200.0);
            paint::label(tr, scene, t, "PROTO", 10.0, hc, cx_proto, hy, 80.0);
            paint::label(tr, scene, t, "ACTION", 10.0, hc, cx_action, hy, 80.0);
            paint::label(tr, scene, t, "SOURCE", 10.0, hc, cx_source, hy, 160.0);
            paint::label(tr, scene, t, "ON", 10.0, hc, cx_ctrl, hy, 60.0);
        }
        paint::hline(
            scene,
            t,
            table.x0 + 12.0,
            table.x1 - 12.0,
            table.y0 + 30.0,
            theme.border_standard,
            1.0,
        );
    }

    /// One rule row: selection highlight, narrow/wide content, then the
    /// shared toggle + delete controls and row separator.
    #[allow(
        clippy::too_many_arguments,
        reason = "per-row paint orchestrator threading the shared scene/transform/theme plus \
                  this row's own geometry and data; a single call site, so bundling further \
                  would only add boilerplate"
    )]
    fn draw_table_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
        cols: (f64, f64, f64, f64, f64),
        row: &RuleRow,
        rule: &Rule,
    ) {
        let selected = self.selected_rule == Some(row.id);
        if selected {
            paint::fill_rrect(
                scene,
                t,
                row.rect,
                8.0,
                theme.tokens.colors.surface_white_10,
            );
            paint::stroke_rrect(scene, t, row.rect, 8.0, theme.accent_primary, 1.0);
        }
        let style = RowStyle::for_rule(theme, rule);
        let src = rule.source_label();

        if l.narrow {
            Self::draw_table_row_narrow(scene, t, tr, cols.0, row, rule, &style, src);
        } else {
            Self::draw_table_row_wide(scene, t, tr, cols, row, rule, &style, src);
        }

        Self::draw_table_row_controls(scene, t, theme, row, rule.enabled);
    }

    /// Two-line row: target + action on top, proto · source beneath. Used
    /// when the table is too narrow for five columns abreast.
    #[allow(
        clippy::too_many_arguments,
        reason = "per-row paint helper threading the shared scene/transform plus this row's \
                  own geometry and precomputed style; a single call site, so bundling further \
                  would only add boilerplate"
    )]
    fn draw_table_row_narrow(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        cx_target: f64,
        row: &RuleRow,
        rule: &Rule,
        style: &RowStyle,
        src: &str,
    ) {
        let dot_y = row.rect.y0 + 18.0;
        paint::dot(
            scene,
            t,
            Point::new(cx_target + 4.0, dot_y),
            3.0,
            if style.dim {
                alpha(style.dot_color, 120)
            } else {
                style.dot_color
            },
        );
        let label_max = row.toggle.x0 - (cx_target + 14.0) - 60.0;
        paint::label(
            tr,
            scene,
            t,
            &rule.matcher.label(),
            14.0,
            style.primary,
            cx_target + 14.0,
            row.rect.y0 + 10.0,
            label_max.max(40.0),
        );
        // Action chip on the top line, just left of the toggle.
        let _ = paint::chip(
            tr,
            scene,
            t,
            style.action_text,
            11.0,
            style.action_color,
            alpha(style.action_color, 40),
            row.toggle.x0 - 62.0,
            dot_y,
            8.0,
            20.0,
            6.0,
        );
        // Second line: proto · source.
        let sub = if src == "anywhere" {
            format!("{} · anywhere", rule.matcher.proto_label())
        } else {
            format!("{} · {}", rule.matcher.proto_label(), src)
        };
        paint::label(
            tr,
            scene,
            t,
            &sub,
            12.0,
            style.secondary,
            cx_target + 14.0,
            row.rect.y0 + 32.0,
            row.rect.width() - 30.0,
        );
    }

    /// Five columns abreast: target, proto, action chip, source.
    #[allow(
        clippy::too_many_arguments,
        reason = "per-row paint helper threading the shared scene/transform plus this row's \
                  own geometry and precomputed style; a single call site, so bundling further \
                  would only add boilerplate"
    )]
    fn draw_table_row_wide(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        cols: (f64, f64, f64, f64, f64),
        row: &RuleRow,
        rule: &Rule,
        style: &RowStyle,
        src: &str,
    ) {
        let (cx_target, cx_proto, cx_action, cx_source, cx_ctrl) = cols;
        let cy = f64::midpoint(row.rect.y0, row.rect.y1);
        paint::dot(
            scene,
            t,
            Point::new(cx_target + 4.0, cy),
            3.0,
            if style.dim {
                alpha(style.dot_color, 120)
            } else {
                style.dot_color
            },
        );
        paint::label_v(
            tr,
            scene,
            t,
            &rule.matcher.label(),
            14.0,
            style.primary,
            cx_target + 14.0,
            row.rect.y0,
            row.rect.height(),
            cx_proto - cx_target - 18.0,
        );
        paint::label_v(
            tr,
            scene,
            t,
            &rule.matcher.proto_label(),
            12.0,
            style.secondary,
            cx_proto,
            row.rect.y0,
            row.rect.height(),
            60.0,
        );
        let _ = paint::chip(
            tr,
            scene,
            t,
            style.action_text,
            11.0,
            style.action_color,
            alpha(style.action_color, 40),
            cx_action,
            cy,
            8.0,
            20.0,
            6.0,
        );
        let src_color = if src == "anywhere" {
            style.secondary
        } else {
            style.primary
        };
        paint::label_v(
            tr,
            scene,
            t,
            src,
            12.0,
            src_color,
            cx_source,
            row.rect.y0,
            row.rect.height(),
            cx_ctrl - cx_source - 8.0,
        );
    }

    /// Enable toggle + delete (×) button + row separator — identical in the
    /// narrow and wide layouts.
    fn draw_table_row_controls(
        scene: &mut Scene,
        t: Affine,
        theme: &Theme,
        row: &RuleRow,
        enabled: bool,
    ) {
        draw_toggle(scene, t, theme, row.toggle, enabled);

        // Delete (×) drawn as two strokes inside a soft danger button.
        let d = row.delete;
        paint::fill_rrect(
            scene,
            t,
            d,
            6.0,
            alpha(theme.tokens.colors.semantic.danger, 30),
        );
        let m = 8.0;
        let dc = theme.tokens.colors.semantic.danger;
        scene.stroke(
            &vello::kurbo::Stroke::new(1.6),
            t,
            dc,
            None,
            &vello::kurbo::Line::new(
                Point::new(d.x0 + m, d.y0 + m),
                Point::new(d.x1 - m, d.y1 - m),
            ),
        );
        scene.stroke(
            &vello::kurbo::Stroke::new(1.6),
            t,
            dc,
            None,
            &vello::kurbo::Line::new(
                Point::new(d.x1 - m, d.y0 + m),
                Point::new(d.x0 + m, d.y1 - m),
            ),
        );

        // Row separator.
        paint::hline(
            scene,
            t,
            row.rect.x0,
            row.rect.x1,
            row.rect.y1 + 2.0,
            alpha(theme.border_standard, 120),
            1.0,
        );
    }

    fn draw_services(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let panel = l.services;
        paint::fill_rect(scene, t, panel, theme.tokens.colors.neutral[0]);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            theme.border_standard,
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0, panel.y0),
                Point::new(panel.x0, panel.y1),
            ),
        );
        let on_count = self
            .config
            .zone(self.selected_zone)
            .map_or(0, |z| z.services.len());
        paint::label(
            tr,
            scene,
            t,
            "SERVICES",
            11.0,
            theme.text_dim,
            panel.x0 + 16.0,
            panel.y0 + 18.0,
            120.0,
        );
        paint::label_right_v(
            tr,
            scene,
            t,
            &format!("{on_count} enabled"),
            11.0,
            theme.accent_primary,
            panel.x1 - 16.0,
            panel.y0,
            36.0,
        );

        let Some(z) = self.config.zone(self.selected_zone) else {
            return;
        };
        for (idx, row, toggle) in &l.service_rows {
            let Some(def) = ServiceCatalog::ALL.get(*idx) else {
                continue;
            };
            let on = z.has_service(def.id);
            if on {
                paint::fill_rrect(scene, t, *row, 8.0, theme.tokens.colors.surface_white_5);
            }
            paint::label(
                tr,
                scene,
                t,
                def.name,
                14.0,
                theme.text_primary,
                row.x0 + 10.0,
                row.y0 + 5.0,
                row.width() - 60.0,
            );
            let detail = format!("{}/{}", def.ports, def.proto);
            paint::label(
                tr,
                scene,
                t,
                &detail,
                11.0,
                theme.text_dim,
                row.x0 + 10.0,
                row.y0 + 24.0,
                row.width() - 60.0,
            );
            draw_toggle(scene, t, theme, *toggle, on);
        }
    }

    fn draw_form(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let card = l.form;
        paint::fill_rrect(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);
        paint::stroke_rrect(scene, t, card, CARD_R, theme.border_standard, 1.0);

        let title = format!("Add rule to {}", self.selected_zone.name());
        paint::label(
            tr,
            scene,
            t,
            &title,
            13.0,
            theme.accent_primary,
            card.x0 + 16.0,
            card.y0 + 12.0,
            300.0,
        );

        // kind segmented.
        let kind_sel = usize::from(!self.form.is_service);
        Self::draw_segmented(
            scene,
            t,
            tr,
            theme,
            &l.kind_seg,
            &["Service", "Port"],
            kind_sel,
        );

        if self.form.is_service {
            self.draw_form_service_selector(scene, t, tr, theme, l);
        } else {
            // port field (TextInput) + proto segmented.
            self.form.port.draw(scene, t, tr, theme, presence);
            let psel = match self.form.proto {
                Proto::Tcp => 0,
                Proto::Udp => 1,
            };
            Self::draw_segmented(scene, t, tr, theme, &l.proto_seg, &["tcp", "udp"], psel);
        }

        // action segmented.
        let asel = match self.form.action {
            Action::Allow => 0,
            Action::Deny => 1,
        };
        Self::draw_segmented(scene, t, tr, theme, &l.action_seg, &["allow", "deny"], asel);

        self.draw_form_source(scene, t, tr, theme, presence, l);
        Self::draw_form_add_button(scene, t, tr, theme, l);

        // Status line (wide only — the stacked form fills its card top to bottom).
        if !self.status.is_empty() && !l.narrow {
            paint::label(
                tr,
                scene,
                t,
                &self.status,
                11.0,
                theme.text_dim,
                card.x0 + 16.0,
                card.y1 - 22.0,
                card.width() - 32.0,
            );
        }
    }

    /// Service-kind matcher control: a cycle selector `[< name >]`.
    fn draw_form_service_selector(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        let svc_id = self.form.current_service();
        let name = ServiceCatalog::name_of(&svc_id);
        paint::fill_rrect(
            scene,
            t,
            l.service_prev,
            7.0,
            theme.tokens.colors.neutral[2],
        );
        paint::fill_rrect(scene, t, l.service_sel, 0.0, theme.tokens.colors.neutral[1]);
        paint::fill_rrect(
            scene,
            t,
            l.service_next,
            7.0,
            theme.tokens.colors.neutral[2],
        );
        paint::label_v(
            tr,
            scene,
            t,
            "<",
            14.0,
            theme.text_primary,
            l.service_prev.x0 + 12.0,
            l.service_prev.y0,
            l.service_prev.height(),
            20.0,
        );
        paint::label_v(
            tr,
            scene,
            t,
            ">",
            14.0,
            theme.text_primary,
            l.service_next.x0 + 12.0,
            l.service_next.y0,
            l.service_next.height(),
            20.0,
        );
        let w = paint::text_width(tr, name, 13.0);
        paint::label_v(
            tr,
            scene,
            t,
            name,
            13.0,
            theme.text_primary,
            l.service_sel.x0 + (l.service_sel.width() - w) / 2.0,
            l.service_sel.y0,
            l.service_sel.height(),
            w + 2.0,
        );
    }

    /// Source label + field. The label sits above the full-width field when
    /// stacked (narrow), and beside it when the form is a single row (wide).
    fn draw_form_source(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        l: &Layout,
    ) {
        let card = l.form;
        if l.narrow {
            paint::label(
                tr,
                scene,
                t,
                "Source",
                11.0,
                theme.text_dim,
                l.source_field.x0,
                l.source_field.y0 - 16.0,
                120.0,
            );
        } else {
            paint::label_v(
                tr,
                scene,
                t,
                "Source",
                12.0,
                theme.text_dim,
                card.x0 + 16.0,
                l.source_field.y0,
                CTRL_H,
                60.0,
            );
        }
        self.form.source.draw(scene, t, tr, theme, presence);
    }

    fn draw_form_add_button(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        paint::fill_rrect(scene, t, l.add_btn, 8.0, theme.accent_primary);
        let at = "Add rule";
        let w = paint::text_width(tr, at, 13.0);
        paint::label_v(
            tr,
            scene,
            t,
            at,
            13.0,
            Color::WHITE,
            l.add_btn.x0 + (l.add_btn.width() - w) / 2.0,
            l.add_btn.y0,
            l.add_btn.height(),
            w + 2.0,
        );
    }
}

/// Resolve the rule-table column x positions from the table rect.
fn rule_columns(table: Rect) -> (f64, f64, f64, f64, f64) {
    let inner_x0 = table.x0 + 12.0;
    let inner_x1 = table.x1 - 12.0;
    let avail = inner_x1 - inner_x0;
    let cx_target = inner_x0;
    let cx_proto = inner_x0 + 0.30 * avail;
    let cx_action = inner_x0 + 0.42 * avail;
    let cx_source = inner_x0 + 0.56 * avail;
    let cx_ctrl = inner_x1 - 92.0;
    (cx_target, cx_proto, cx_action, cx_source, cx_ctrl)
}

fn target_color(theme: &Theme, target: Target) -> Color {
    match target {
        Target::Accept => theme.tokens.colors.semantic.success,
        Target::Default | Target::Reject => theme.tokens.colors.semantic.warning,
        Target::Drop => theme.tokens.colors.semantic.danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_rect() -> FirewallApp {
        // Sample-populated build: these tests exercise rule/zone behaviour
        // that needs a ruleset, which only the demo build fabricates.
        let mut a = FirewallApp::new_demo();
        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        a.relayout();
        a
    }

    #[test]
    fn live_new_does_not_fabricate_sample_config() {
        // The honesty guard: the live constructor must never seed the demo
        // sample. On a host without firewalld it is empty + unavailable; on
        // one with firewalld it reflects the real host — either way it is
        // not the fabricated sample ruleset.
        let live = FirewallApp::new();
        assert_ne!(
            live.config,
            crate::sample::sample_config(),
            "live app must not start on the fabricated sample firewall"
        );
    }

    #[test]
    fn unavailable_firewall_writes_nothing_to_doc() {
        // With no readable firewall the config is empty and `sync` must not
        // publish a fabricated ruleset into the shared document.
        let mut a = FirewallApp::from_parts(FirewallConfig::empty(), false);
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        assert!(!a.available);
        assert!(a.config.zones.is_empty());
        assert!(
            FirewallDoc::load(&doc).is_none(),
            "the unavailable state must leave the document empty, not seed a config"
        );
    }

    #[test]
    fn demo_build_publishes_sample_for_shots() {
        // The `--shots` build still populates so the screenshot is real.
        let mut a = FirewallApp::new_demo();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let stored = FirewallDoc::load(&doc).expect("demo build seeds the document");
        // The demo build publishes the populated sample (so the --shots
        // screenshot is real) — distinct from the live app, which publishes
        // nothing. Assert on the sample's zone set (robust to store→load field
        // normalisation like empty→"0" ports that can break exact equality).
        let sample = crate::sample::sample_config();
        assert!(
            !stored.zones.is_empty(),
            "demo publishes a populated firewall"
        );
        assert_eq!(
            stored.zones.len(),
            sample.zones.len(),
            "demo publishes every sample zone"
        );
    }

    #[test]
    fn relayout_produces_all_nine_zone_rows() {
        let a = app_with_rect();
        assert_eq!(a.layout.zone_rows.len(), 9);
    }

    #[test]
    fn filter_restricts_rule_rows() {
        let mut a = app_with_rect();
        a.filter = Filter::Deny;
        a.relayout();
        // public has 3 deny rules in the sample.
        let denies = a
            .selected_zone_ref()
            .unwrap()
            .rules
            .iter()
            .filter(|r| r.action == Action::Deny)
            .count();
        assert_eq!(a.layout.rule_rows.len(), denies);
        assert!(denies > 0);
    }

    #[test]
    fn add_rule_from_form_appends_and_selects() {
        let mut a = app_with_rect();
        let mut doc = AutoCommit::new();
        // Actions read the shared config from the document, so seed it first —
        // the same publish `sync` does on the app's first frame.
        a.sync(&mut doc, &automerge::ROOT);
        let before = a.selected_zone_ref().unwrap().rules.len();
        a.form.is_service = false;
        a.form.port.set_text("9000");
        a.form.action = Action::Allow;
        a.form.source.set_text("");
        a.add_rule_from_form(&mut doc);
        let after = a.selected_zone_ref().unwrap().rules.len();
        assert_eq!(after, before + 1);
        assert!(a.selected_rule.is_some());
        let last = a.selected_zone_ref().unwrap().rules.last().unwrap();
        assert_eq!(
            last.matcher,
            Matcher::Port {
                port: "9000".into(),
                proto: Proto::Tcp
            }
        );
    }

    #[test]
    fn toggle_service_round_trips_through_doc() {
        let mut a = app_with_rect();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        // ssh (idx 0) is enabled on public in the sample.
        let was = a.selected_zone_ref().unwrap().has_service("ssh");
        a.toggle_service(0, &mut doc);
        let now = a.selected_zone_ref().unwrap().has_service("ssh");
        assert_ne!(was, now);
        // Persisted JSON reflects the change.
        let reloaded = FirewallDoc::load(&doc).unwrap();
        assert_eq!(
            reloaded.zone(ZoneKind::Public).unwrap().has_service("ssh"),
            now
        );
    }

    #[test]
    fn set_active_zone_changes_default() {
        let mut a = app_with_rect();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        a.select_zone(ZoneKind::Home);
        a.set_active_zone(&mut doc);
        assert_eq!(a.config.default_zone, ZoneKind::Home);
        // The change lands in the shared document, not just the in-memory view.
        assert_eq!(
            FirewallDoc::load(&doc).unwrap().default_zone,
            ZoneKind::Home
        );
    }

    #[test]
    fn cycle_zone_wraps() {
        let mut a = app_with_rect();
        a.select_zone(ZoneKind::ALL[0]);
        a.cycle_zone(-1);
        assert_eq!(a.selected_zone, ZoneKind::ALL[8]);
        a.cycle_zone(1);
        assert_eq!(a.selected_zone, ZoneKind::ALL[0]);
    }

    #[test]
    fn delete_rule_removes_from_zone() {
        let mut a = app_with_rect();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let id = a.selected_zone_ref().unwrap().rules[0].id;
        let before = a.selected_zone_ref().unwrap().rules.len();
        a.delete_rule(id, &mut doc);
        assert_eq!(a.selected_zone_ref().unwrap().rules.len(), before - 1);
        // …and the shared document agrees.
        assert!(
            !FirewallDoc::load(&doc)
                .unwrap()
                .zone(a.selected_zone)
                .unwrap()
                .rules
                .iter()
                .any(|r| r.id == id),
            "the deleted rule is gone from the document too"
        );
    }

    /// One source of truth: the real trash-button path (`delete_rule`) dispatches
    /// the byte-identical [`EncodedAction`] the palette's per-rule "delete" spec
    /// builds. Proven on the production method, not a reconstruction of it.
    #[test]
    fn delete_button_dispatches_the_palette_action() {
        use qos_actions::ActionInput;

        let mut a = app_with_rect();
        let mut doc = AutoCommit::new();
        a.sync(&mut doc, &automerge::ROOT);
        let zone = a.selected_zone;
        let id = a.selected_zone_ref().unwrap().rules[0].id;

        // The palette's delete spec for this exact rule.
        let resources = Widget::resources(&a, &doc);
        let spec_encoded = resources[0]
            .children
            .iter()
            .find(|z| z.id.as_str() == format!("firewall.zone:{}", zone.name()))
            .and_then(|z| {
                z.children
                    .iter()
                    .find(|r| r.id.as_str() == format!("firewall.rule:{id}"))
            })
            .and_then(|r| r.actions.iter().find(|act| act.id == "delete"))
            .map(|act| match &act.input {
                ActionInput::Immediate(enc) => enc.clone(),
                ActionInput::Prompted { .. } => panic!("delete is an immediate action"),
            })
            .expect("a delete spec for the rule");

        // The real button path records what it dispatched.
        a.delete_rule(id, &mut doc);
        let dispatched = a
            .last_dispatched()
            .cloned()
            .expect("the button dispatched an action");
        assert_eq!(
            dispatched, spec_encoded,
            "button and palette are one action"
        );
    }
}
