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
//! State lives in the shared Automerge document (`doc.rs`): every edit is a
//! [`FirewallAction`] submitted through the framework dispatcher, and the
//! framework presents the stored configuration back (see
//! [`crate::resource`]), so two instances on one record edit the same
//! ruleset.

use std::collections::HashMap;
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;
use winit::event::{ElementState, Modifiers, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::a11y::{AccessNode, AccessState, Action as A11yAction, Politeness, Role};
use ui::chrome::{
    PANEL_BORDER_ALPHA, SelectionState, chrome_hairline, panel_border, panel_ground, selection_tier,
};
use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::Spacing;
use ui::widget::{
    Button, ButtonFace, ListSemanticRow, SegmentedFace, SidebarDrawer, TableSemanticCell,
    TableSemanticColumn, TableSemanticControl, TableSemanticRow, TextInput, Toggle, ToggleFace,
    ToggleGroup, VirtualListSemantics, VirtualTableSemantics, Widget,
};

use crate::actions::FirewallAction;
use crate::backend::{Backend, Mode};
use crate::model::{
    Action, FirewallConfig, Matcher, Proto, Rule, RuleId, ServiceCatalog, Target, Zone, ZoneKind,
    validate_port, validate_source,
};
use crate::paint;
use crate::resource::{AppliedConfig, ApplyOutcome, Firewall, FirewallEffect};
use ui::framework::{Dispatcher, Setting};

// ---- geometry constants -------------------------------------------------
//
// Every band below is the 1× *floor* of a measured value in [`Metrics`]: the
// layout reads the metrics, which never go under these constants, so the 1×
// picture is unchanged and the bands only grow once the accessibility text
// scale makes their labels taller than the band was drawn for.
const HEADER_H: f64 = 60.0;
const RAIL_W: f64 = 214.0;
const SERVICES_W: f64 = 300.0;
const CARD_R: f64 = 12.0;

// ---- spacing rhythm (HIG §7b) ------------------------------------------
//
// One rhythm for the whole screen, read from the unscaled token scale so a
// gap in this file is always a named step: `S1` between a row and its rule
// or a label and its control, `S2` between related rows, `S3` between
// siblings (controls on one form line, the filter row and the table), `S4`
// for card padding and the gap between cards, `S6` before the next group.
const S1: f64 = Spacing::BASE.s1 as f64;
const S2: f64 = Spacing::BASE.s2 as f64;
const S3: f64 = Spacing::BASE.s3 as f64;
const S4: f64 = Spacing::BASE.s4 as f64;
/// Card padding and the gap between cards / regions.
const PAD: f64 = S4;
/// Summary card floor: title, description and one row of stat chips. The
/// card grows per extra chip row instead of reserving a fixed taller band
/// (the old `SUMMARY_H_NARROW = 188` left ~60px blank when the chips fit).
/// `S4` padding above the 27.5px title, `S1` to the 15px description, `S3`
/// to the 40px chip row, `S4` below it.
const SUMMARY_H: f64 = S4 + 27.5 + S1 + 15.0 + S3 + 40.0 + S4;
/// Add-rule form floor: title band, two control lines `S3` apart and the
/// status band.
const FORM_H: f64 = 42.0 + 2.0 * CTRL_H + S3 + 30.0;
/// The add-rule form stacks its controls onto four lines when narrow.
const FORM_H_NARROW: f64 = 226.0;
const RULE_ROW_H: f64 = 40.0;
/// Two-line rule rows (target + action on top, proto · source beneath) when
/// the table is too narrow for five abreast columns.
const RULE_ROW_H_NARROW: f64 = 58.0;
const SVC_ROW_H: f64 = 46.0;
/// Dense but still two-line-readable rows let every catalog service fit in the
/// compact Services route without making the window scroll-dependent.
const SVC_ROW_H_NARROW: f64 = 30.0;
const CTRL_H: f64 = 34.0;
const TOGGLE_SIZE: f64 = 34.0;

/// Below this window width the zone rail overlays (a summoned drawer) instead
/// of docking, the add-rule form stacks its controls, and the rules table
/// switches to two-line rows. Scaled by the text scale: an 800px window at 2×
/// has the room of a 400px one, so it takes the narrow layout.
const NARROW_BREAKPOINT: f64 = 720.0;
/// The per-zone services panel only earns its ~300px column at/above this
/// width; below it collapses so the rules table + form get the whole center.
const SERVICES_BREAKPOINT: f64 = 1000.0;

/// Text-derived geometry. Refreshed from the renderer in
/// `adjust_layout_with_measure` and again at the top of `draw`, then read by
/// [`FirewallApp::relayout`], which has no renderer of its own. Every band
/// floors at the 1× value in [`Metrics::default`] — the geometry the layout
/// used before it was measured — so hit rects and painted rects come from
/// one place at every text scale.
#[derive(Clone, Debug, PartialEq)]
struct Metrics {
    text_scale: f64,
    /// Control band: form controls and the header buttons (`CTRL_H` floor).
    ctrl_h: f64,
    header_h: f64,
    /// Header button widths: enable pill, panic, apply — wide and narrow
    /// forms, each its 1× floor grown to its label plus padding.
    enable_w: f64,
    panic_w: f64,
    apply_w: f64,
    enable_w_narrow: f64,
    panic_w_narrow: f64,
    apply_w_narrow: f64,
    rail_rows_top: f64,
    rail_row_h: f64,
    /// Summary card: measured title / description line heights and, per
    /// zone, each stat chip's width in [`stat_chip_specs`] order — every zone
    /// is measured so a zone-change click relayouts against the right widths
    /// before the next paint re-measures.
    title_h: f64,
    desc_h: f64,
    chip_h: f64,
    /// Gap between chips on a row, and between chip rows.
    chip_gap: f64,
    chip_row_gap: f64,
    chip_ws: Vec<(ZoneKind, Vec<f64>)>,
    seg_h: f64,
    /// Filter (`All`/`Allow`/`Deny`) and route (`Rules`/`Services`) segment
    /// widths: wide / narrow floors, grown to the widest label.
    filter_seg_w: f64,
    filter_seg_w_narrow: f64,
    content_seg_w: f64,
    table_head_h: f64,
    rule_row_h: f64,
    rule_row_h_narrow: f64,
    /// Two-line row: centre of the dot/action chip on the first line and the
    /// top of the second line.
    rule_dot_y: f64,
    rule_sub_top: f64,
    svc_row_h: f64,
    svc_row_h_narrow: f64,
    /// Services row: top of the `ports/proto` detail line under the name.
    svc_detail_top: f64,
    /// Form: title band above line 1 (wide / stacked), the status band under
    /// the last line (wide), and the gap that carries the `Source` caption
    /// above its field when stacked.
    form_title_h: f64,
    form_title_h_narrow: f64,
    form_status_h: f64,
    form_source_gap: f64,
    /// Form control widths that follow their labels (1× floors).
    kind_w: f64,
    act_w: f64,
    proto_w: f64,
    add_w: f64,
}

impl Default for Metrics {
    /// The unmeasured 1× layout — the single source of every band's 1× floor
    /// (the geometry the layout used before it was measured, plus the two
    /// offsets `measure` derives from the 1× line height: the two-line row's
    /// dot centre and the stacked form's title band), so the first frame lays
    /// out exactly as every measured 1× frame does.
    fn default() -> Self {
        Self {
            text_scale: 1.0,
            ctrl_h: CTRL_H,
            header_h: HEADER_H,
            enable_w: 92.0,
            panic_w: 104.0,
            apply_w: 92.0,
            enable_w_narrow: 80.0,
            panic_w_narrow: 78.0,
            apply_w_narrow: 76.0,
            // Zone rail: first row under the `ZONES` caption, row pitch.
            rail_rows_top: 44.0,
            rail_row_h: 32.0,
            title_h: 27.5,
            desc_h: 15.0,
            chip_h: 40.0,
            chip_gap: 10.0,
            chip_row_gap: 8.0,
            chip_ws: Vec::new(),
            // Filter / Rules·Services segments.
            seg_h: 30.0,
            filter_seg_w: 72.0,
            filter_seg_w_narrow: 60.0,
            content_seg_w: 66.0,
            // Rules-table caption band above the separator line.
            table_head_h: 30.0,
            rule_row_h: RULE_ROW_H,
            rule_row_h_narrow: RULE_ROW_H_NARROW,
            rule_dot_y: 18.75,
            rule_sub_top: 32.0,
            svc_row_h: SVC_ROW_H,
            svc_row_h_narrow: SVC_ROW_H_NARROW,
            svc_detail_top: 24.0,
            form_title_h: 42.0,
            form_title_h_narrow: 50.0,
            form_status_h: 30.0,
            form_source_gap: 26.0,
            kind_w: 64.0,
            act_w: 64.0,
            proto_w: 52.0,
            add_w: 120.0,
        }
    }
}

impl Metrics {
    /// Measure every band from the renderer at its current text scale. Each
    /// value is the label's measured line plus the padding the 1× band
    /// implied, floored at that band ([`Self::default`]). `chips` are every
    /// zone's stat chips ([`stat_chip_specs`]).
    fn measure(tr: &mut TextRenderer, chips: &[ZoneChips]) -> Self {
        let floors = Self::default();
        let h = |tr: &mut TextRenderer, size: f32| f64::from(tr.measure("Hg", size, None).1);
        let w = |tr: &mut TextRenderer, text: &str, size: f32| paint::text_width(tr, text, size);
        let h9 = h(tr, 9.0);
        let h10 = h(tr, 10.0);
        let h11 = h(tr, 11.0);
        let h12 = h(tr, 12.0);
        let h13 = h(tr, 13.0);
        let h14 = h(tr, 14.0);
        let h22 = h(tr, 22.0);
        let ctrl_h = CTRL_H.max(h13 + 16.0);
        let rule_dot_y = 10.0 + h14 / 2.0;
        let rule_sub_top = 10.0 + h14 + 4.5;
        let svc_detail_top = 5.0 + h14 + 1.5;
        let chip_ws = chips
            .iter()
            .map(|(zone, specs)| {
                let ws = specs
                    .iter()
                    .map(|(caption, value, _)| w(tr, caption, 9.0).max(w(tr, value, 13.0)) + 20.0)
                    .collect();
                (*zone, ws)
            })
            .collect();
        let filter_label_w = Filter::ALL
            .iter()
            .map(|f| w(tr, f.label(), 13.0))
            .fold(0.0, f64::max);
        let content_label_w = w(tr, "Rules", 13.0).max(w(tr, "Services", 13.0));
        // Header buttons: the narrow form keeps a tighter floor but the same
        // minimum label padding, so a long label never outgrows its button.
        let enable_label = w(tr, "○ Stopped", 12.0).max(w(tr, "● Active", 12.0));
        let panic_label = w(tr, "Panic ON", 12.0);
        let apply_label = w(tr, "Apply", 13.0);
        Self {
            text_scale: f64::from(tr.text_scale()),
            ctrl_h,
            header_h: HEADER_H.max(ctrl_h + 26.0),
            enable_w: 92.0_f64.max(enable_label + 24.0),
            panic_w: 104.0_f64.max(panic_label + 24.0),
            apply_w: 92.0_f64.max(apply_label + 24.0),
            enable_w_narrow: floors.enable_w_narrow.max(enable_label + 16.0),
            panic_w_narrow: floors.panic_w_narrow.max(panic_label + 16.0),
            apply_w_narrow: floors.apply_w_narrow.max(apply_label + 16.0),
            rail_rows_top: floors.rail_rows_top.max(18.0 + h11 + 12.0),
            rail_row_h: floors.rail_row_h.max(h14 + 10.0),
            title_h: h22,
            desc_h: h12,
            chip_h: floors.chip_h.max(h9 + h13 + 12.0),
            chip_gap: floors.chip_gap,
            chip_row_gap: floors.chip_row_gap,
            chip_ws,
            seg_h: floors.seg_h.max(h13 + 12.0),
            filter_seg_w: floors.filter_seg_w.max(filter_label_w + 20.0),
            filter_seg_w_narrow: floors.filter_seg_w_narrow.max(filter_label_w + 20.0),
            content_seg_w: floors.content_seg_w.max(content_label_w + 20.0),
            table_head_h: floors.table_head_h.max(h10 + 14.0),
            rule_row_h: RULE_ROW_H.max(h14 + 20.0),
            rule_row_h_narrow: RULE_ROW_H_NARROW.max(rule_sub_top + h12 + 11.0),
            rule_dot_y,
            rule_sub_top,
            svc_row_h: SVC_ROW_H.max(svc_detail_top + h10 + 9.5),
            svc_row_h_narrow: SVC_ROW_H_NARROW.max(3.0 + h13 + 5.0),
            svc_detail_top,
            form_title_h: 42.0_f64.max(12.0 + h13 + 12.0),
            // Stacked form: the status line sits between the title and line
            // 1, so the band carries both lines (1× grows 40 → 50; before,
            // a validation message painted across the title's descenders).
            form_title_h_narrow: 40.0_f64.max(12.0 + h13 + 2.0 + h11 + 6.0),
            form_status_h: 30.0_f64.max(h11 + 16.0),
            form_source_gap: 26.0_f64.max(h11 + 12.0),
            kind_w: 64.0_f64.max(w(tr, "Service", 13.0).max(w(tr, "Port", 13.0)) + 20.0),
            act_w: 64.0_f64.max(w(tr, "allow", 13.0).max(w(tr, "deny", 13.0)) + 20.0),
            proto_w: 52.0_f64.max(w(tr, "tcp", 13.0).max(w(tr, "udp", 13.0)) + 20.0),
            add_w: 120.0_f64.max(w(tr, "Add rule", 13.0) + 32.0),
        }
    }

    /// The measured stat-chip widths for `zone` (empty until measured).
    fn chip_ws(&self, zone: ZoneKind) -> &[f64] {
        self.chip_ws
            .iter()
            .find_map(|(z, ws)| (*z == zone).then_some(ws.as_slice()))
            .unwrap_or(&[])
    }

    /// Height of the summary card for `rows` rows of stat chips: title,
    /// description, the chip rows and the card's bottom padding.
    fn summary_h(&self, rows: usize) -> f64 {
        let rows = rows.max(1) as f64;
        SUMMARY_H.max(self.chips_top() + rows * self.chip_h + (rows - 1.0) * self.chip_row_gap + S4)
    }

    /// The description follows the title by `S1` (a label and the line
    /// that explains it), under the card's `S4` padding.
    fn desc_top(&self) -> f64 {
        S4 + self.title_h + S1
    }

    /// The chip row is the next sibling group: `S3` under the description.
    fn chips_top(&self) -> f64 {
        self.desc_top() + self.desc_h + S3
    }
}

/// Colour role of a summary stat value, resolved against the theme at paint
/// time so the chip list can be built (and measured) without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChipTone {
    Target(Target),
    Primary,
    Success,
    Danger,
    Info,
}

/// The summary card's stat chips for `z`, in display order. One list feeds
/// the measurement, the wrapping layout and the painter, so a chip is always
/// painted where the layout wrapped it.
/// One stat chip: its label, its value text and its tone.
type StatChip = (&'static str, String, ChipTone);
/// A zone's stat chips, keyed by the zone they describe.
type ZoneChips = (ZoneKind, Vec<StatChip>);

fn stat_chip_specs(z: &Zone) -> Vec<StatChip> {
    let (allow, deny) = z.rule_counts();
    let ifaces = if z.interfaces.is_empty() {
        "—".to_string()
    } else {
        z.interfaces.join(", ")
    };
    let mut chips = vec![
        (
            "target",
            z.target.short().to_string(),
            ChipTone::Target(z.target),
        ),
        ("services", z.services.len().to_string(), ChipTone::Primary),
        ("allow rules", allow.to_string(), ChipTone::Success),
        ("deny rules", deny.to_string(), ChipTone::Danger),
        ("interfaces", ifaces, ChipTone::Primary),
    ];
    if z.masquerade {
        chips.push(("nat", "masquerade".to_string(), ChipTone::Info));
    }
    chips
}

/// Which rules are shown in the table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    All,
    Allow,
    Deny,
}

/// Local-only content route used when the desktop services column cannot fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompactView {
    Rules,
    Services,
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
    recover_btn: Rect,
    enable_pill: Rect,
    panic_btn: Rect,
    apply_btn: Rect,

    rail: Rect,
    rail_toggle: Rect,
    zone_rows: Vec<(ZoneKind, Rect)>,

    summary: Rect,
    set_active_btn: Rect,
    /// One rect per entry of [`stat_chip_specs`], wrapped inside the summary
    /// card; the painter reads these instead of re-deriving positions.
    stat_chips: Vec<Rect>,

    filter_seg: Vec<Rect>,
    content_seg: Vec<Rect>,
    compact_services: bool,
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
    compact_view: CompactView,
    form: FormState,
    /// Whether applies on this device run or are only planned.
    mode: Mode,
    status: String,

    /// The zone rail as the shared adaptive drawer: docked beside the content
    /// on wide windows, a summoned overlay drawer when narrow.
    rail: SidebarDrawer,

    rect: Rect,
    node: Option<NodeId>,
    layout: Layout,
    /// Renderer-measured bands; see [`Metrics`]. `metrics_key` is the
    /// (text scale, config) the metrics were measured for, so a frame that
    /// changes neither skips the measurement.
    metrics: Metrics,
    metrics_key: Option<(u32, String)>,
    last_cursor: Point,

    /// Whether there is a configuration to show. `false` means no device
    /// could read firewalld into the record yet, and we show an honest
    /// unavailable state instead of any fabricated ruleset.
    available: bool,

    /// A locally armed destructive action. The first trash tap selects this
    /// rule; the second, explicit confirmation performs the shared mutation.
    pending_delete: Option<RuleId>,

    /// Where every on-screen affordance submits its [`FirewallAction`]: one
    /// undo step each, replayed byte-identically on peers — the same door
    /// Ctrl+K takes. `None` only for a widget no resource hosts.
    dispatcher: Option<Dispatcher<Firewall>>,
    /// What this device last applied to its firewalld.
    applied: Option<Setting<AppliedConfig>>,
    /// The result of the latest apply on this device, until the status line
    /// shows it.
    outcome: ApplyOutcome,
}

impl Default for FirewallApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FirewallApp {
    /// The surface before the resource presents its configuration: an
    /// honest unavailable state, never a fabricated ruleset.
    pub fn new() -> Self {
        Self::from_parts(FirewallConfig::empty(), false)
    }

    /// The bare widget showing the sample ruleset, for tests of its layout
    /// and input.
    #[cfg(test)]
    pub(crate) fn new_demo() -> Self {
        Self::from_parts(crate::sample::sample_config(), true)
    }

    /// Presentation-only screenshot states, staged once the scenario's
    /// configuration is shown: empty, invalid, confirmation and changed
    /// states for visual review.
    pub(crate) fn pose_fixture(&mut self, name: &str) {
        match name {
            "empty" => {
                self.selected_zone = ZoneKind::Drop;
                self.status =
                    "No custom rules in this zone — add only what this network needs".into();
            }
            "error" => {
                self.selected_zone = ZoneKind::Public;
                self.form.is_service = false;
                self.form.port.set_text("70000");
                self.form.source.set_text("not-a-network");
                self.status = "Rule not added: use a port from 1 to 65535".into();
            }
            "confirmation" => {
                self.selected_zone = ZoneKind::Public;
                self.pending_delete = self
                    .selected_zone_ref()
                    .and_then(|zone| zone.rules.first())
                    .map(|rule| rule.id);
                self.status =
                    "Delete rule? Click its trash button again to confirm (Esc cancels)".into();
            }
            "active-change" => {
                self.selected_zone = ZoneKind::Home;
                self.status = "home is now the active zone — apply when ready".into();
            }
            "service-error" => {
                self.status = "Permission denied while contacting firewalld. Check administrator authorization, then retry.".into();
            }
            "services" => {
                self.compact_view = CompactView::Services;
                self.status = "Showing services (press S for rules)".into();
            }
            // A selected rule row: the selection face on the raised table card.
            "selected" => {
                self.selected_zone = ZoneKind::Public;
                self.selected_rule = self
                    .selected_zone_ref()
                    .and_then(|zone| zone.rules.get(1))
                    .map(|rule| rule.id);
            }
            _ => {}
        }
    }

    fn from_parts(config: FirewallConfig, available: bool) -> Self {
        let selected_zone = config.default_zone;
        Self {
            config,
            selected_zone,
            selected_rule: None,
            filter: Filter::All,
            compact_view: CompactView::Rules,
            form: FormState::new(),
            mode: Backend::detect().mode(),
            status: String::new(),
            rail: SidebarDrawer::new()
                .left()
                .docked_width(RAIL_W)
                .overlay_width(RAIL_W.max(264.0))
                .breakpoint(NARROW_BREAKPOINT),
            rect: Rect::ZERO,
            node: None,
            layout: Layout::default(),
            metrics: Metrics::default(),
            metrics_key: None,
            last_cursor: Point::ZERO,
            available,
            pending_delete: None,
            dispatcher: None,
            applied: None,
            outcome: ApplyOutcome::default(),
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

    /// Submit one [`FirewallAction`] — the single write path shared with the
    /// Ctrl+K palette. The framework applies it after the event and presents
    /// the stored configuration back, which the next frame paints.
    ///
    /// This is *only* the document edit. Pushing the ruleset to the kernel
    /// (`firewall-cmd`/`nft`) follows from the stored configuration on each
    /// device — never part of an action.
    fn dispatch(&self, action: &FirewallAction) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.apply(action);
        }
    }

    /// Invoke one of the firewall's effects through the same door as the
    /// palette.
    fn invoke(&self, effect: &FirewallEffect) {
        if let Some(dispatcher) = &self.dispatcher {
            dispatcher.effect(effect);
        }
    }

    /// Show the stored configuration. An invalid one is not shown, so no
    /// control acts on it.
    pub(crate) fn show_config(&mut self, state: &FirewallConfig) {
        if state.zones.is_empty() {
            self.config = state.clone();
            self.available = false;
            return;
        }
        if state.validate().is_err() {
            self.status = "Shared firewall configuration was rejected as invalid".into();
            return;
        }
        self.config = state.clone();
        self.available = true;
        if self.config.zone(self.selected_zone).is_none() {
            self.selected_zone = self.config.default_zone;
        }
        if let Some(rule) = self.pending_delete
            && !self
                .config
                .zones
                .iter()
                .any(|zone| zone.rules.iter().any(|r| r.id == rule))
        {
            self.pending_delete = None;
        }
    }

    /// Take part in the resource: edits go through `dispatcher`, and
    /// `applied` remembers what this device's firewalld holds.
    pub(crate) fn attach_framework(
        &mut self,
        dispatcher: Dispatcher<Firewall>,
        applied: Setting<AppliedConfig>,
    ) {
        self.dispatcher = Some(dispatcher);
        self.applied = Some(applied);
    }

    /// Where the projection reports how the latest apply on this device
    /// ended.
    pub(crate) fn apply_outcome(&self) -> ApplyOutcome {
        std::rc::Rc::clone(&self.outcome)
    }

    /// Whether there is a configuration to edit and apply.
    pub(crate) fn is_available(&self) -> bool {
        self.available
    }

    // ---- layout ---------------------------------------------------------
    /// Refresh [`Metrics`] from the renderer. Called from the renderer-aware
    /// layout hook and again at the top of `draw`, so the geometry every hit
    /// rect is built from has seen the current text scale and config. Skipped
    /// while neither has changed since the last measurement: the bands
    /// depend only on the text scale, the chip widths only on the config.
    fn measure_metrics(&mut self, tr: &mut TextRenderer) {
        let key = (tr.text_scale().to_bits(), self.config.to_json());
        if self.metrics_key.as_ref() == Some(&key) {
            return;
        }
        let chips: Vec<ZoneChips> = self
            .config
            .zones
            .iter()
            .map(|z| (z.kind, stat_chip_specs(z)))
            .collect();
        self.metrics = Metrics::measure(tr, &chips);
        self.metrics_key = Some(key);
    }

    fn relayout(&mut self) {
        let r = self.rect;
        let mut l = Layout::default();
        if r.width() < 4.0 || r.height() < 4.0 {
            self.layout = l;
            return;
        }

        // Breakpoints follow the text scale: at 2× an 800px window carries
        // what a 400px window carries at 1×, so it takes the narrow layout.
        let scale = self.metrics.text_scale;
        let narrow = r.width() < NARROW_BREAKPOINT * scale;
        let services_visible = r.width() >= SERVICES_BREAKPOINT * scale;
        l.narrow = narrow;
        l.services_visible = services_visible;
        l.compact_services = !services_visible && self.compact_view == CompactView::Services;

        let by = self.layout_header(r, &mut l, narrow);
        let unavailable_area = Rect::new(r.x0, l.header.y1, r.x1, r.y1);
        let card_w = 520.0_f64.min((unavailable_area.width() - 48.0).max(160.0));
        let card_x = unavailable_area.x0 + (unavailable_area.width() - card_w) / 2.0;
        let card_y = unavailable_area.y0 + (unavailable_area.height() - 220.0) / 2.0;
        l.recover_btn = Rect::new(
            card_x + 24.0,
            card_y + 172.0,
            card_x + 184.0,
            card_y + 206.0,
        );
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
    fn layout_header(&self, r: Rect, l: &mut Layout, narrow: bool) -> f64 {
        let m = &self.metrics;
        l.header = Rect::new(r.x0, r.y0, r.x1, r.y0 + m.header_h);
        let by = r.y0 + m.header_h / 2.0;
        let half = m.ctrl_h / 2.0;
        // Narrow windows take the tighter button floors; either form grows
        // with its label.
        let mut rx = r.x1 - PAD;
        let apply_w = if narrow { m.apply_w_narrow } else { m.apply_w };
        l.apply_btn = Rect::new(rx - apply_w, by - half, rx, by + half);
        rx = l.apply_btn.x0 - 8.0;
        let panic_w = if narrow { m.panic_w_narrow } else { m.panic_w };
        l.panic_btn = Rect::new(rx - panic_w, by - half, rx, by + half);
        rx = l.panic_btn.x0 - 8.0;
        let enable_w = if narrow {
            m.enable_w_narrow
        } else {
            m.enable_w
        };
        l.enable_pill = Rect::new(rx - enable_w, by - half, rx, by + half);
        by
    }

    /// Resolve the rail drawer (docked vs overlay), its toggle, and the zone
    /// rows within it. Returns the content rect the rail insets.
    fn layout_rail(&mut self, r: Rect, l: &mut Layout, by: f64) -> Rect {
        // Content area below the header. The rail drawer decides docked (wide)
        // vs overlay (narrow) from its width, insetting the content when docked;
        // its breakpoint follows the same text-scaled rule as `narrow`.
        let content_outer = Rect::new(r.x0, l.header.y1, r.x1, r.y1);
        self.rail.set_docked_width(RAIL_W);
        self.rail.set_overlay_width(RAIL_W.max(264.0));
        self.rail
            .set_breakpoint(NARROW_BREAKPOINT * self.metrics.text_scale);
        let content = self.rail.resolve(content_outer);
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
            let rows_top = l.rail.y0 + self.metrics.rail_rows_top;
            let row_h = self.metrics.rail_row_h;
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
    #[expect(
        clippy::too_many_lines,
        reason = "places every centre-panel band in reading order, top to bottom"
    )]
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

        // Summary card: wrap the stat chips inside the card's width first, so
        // the card is exactly as tall as its rows need (one row at desktop
        // width, two on a phone) instead of a fixed per-breakpoint band.
        let m = &self.metrics;
        let inset_x = cx0 + PAD;
        let max_x = cx1 - PAD;
        let mut sx = inset_x;
        let mut row = 0_usize;
        let chip_ws = m.chip_ws(self.selected_zone);
        let mut chips = Vec::with_capacity(chip_ws.len());
        for &w in chip_ws {
            if sx + w > max_x && sx > inset_x + 0.5 {
                row += 1;
                sx = inset_x;
            }
            let top = cy + m.chips_top() + row as f64 * (m.chip_h + m.chip_row_gap);
            chips.push(Rect::new(sx, top, sx + w, top + m.chip_h));
            sx += w + m.chip_gap;
        }
        let rows = if chips.is_empty() { 1 } else { row + 1 };
        l.summary = Rect::new(cx0, cy, cx1, cy + m.summary_h(rows));
        l.stat_chips = chips;
        // Set-active button inside summary, top-right.
        l.set_active_btn = Rect::new(
            l.summary.x1 - 12.0 - 118.0,
            l.summary.y0 + 12.0,
            l.summary.x1 - 12.0,
            l.summary.y0 + 12.0 + 30.0,
        );

        // Filter row.
        let fy = l.summary.y1 + PAD;
        let seg_w = if narrow {
            m.filter_seg_w_narrow
        } else {
            m.filter_seg_w
        };
        let seg_h = m.seg_h;
        let seg_total = seg_w * 3.0;
        for i in 0..3 {
            let x = cx1 - seg_total + f64::from(i) * seg_w;
            l.filter_seg.push(Rect::new(x, fy, x + seg_w, fy + seg_h));
        }
        if !services_visible {
            let compact_w = m.content_seg_w;
            for i in 0..2 {
                let x = cx0 + f64::from(i) * compact_w;
                l.content_seg
                    .push(Rect::new(x, fy, x + compact_w, fy + seg_h));
            }
        }

        // Form card pinned to the bottom (taller when its controls stack). Its
        // height is the sum of the bands its controls occupy, so the table
        // above it gets every pixel the form does not need.
        let form_h = if narrow {
            FORM_H_NARROW.max(
                m.form_title_h_narrow + 3.0 * m.ctrl_h + 12.0 + m.form_source_gap + 12.0 + m.ctrl_h,
            )
        } else {
            FORM_H.max(m.form_title_h + 2.0 * m.ctrl_h + S3 + m.form_status_h)
        };
        l.form = if l.compact_services {
            Rect::ZERO
        } else {
            Rect::new(cx0, cy1 - form_h, cx1, cy1)
        };

        // Rules table fills the gap, `S3` under the filter row it belongs to.
        l.table = Rect::new(
            cx0,
            fy + seg_h + S3,
            cx1,
            if l.compact_services {
                cy1
            } else {
                l.form.y0 - PAD
            },
        );
        if !l.compact_services {
            self.build_table_rows(l, narrow);
        }

        // Form internals.
        if !l.compact_services {
            self.build_form(l, narrow);
        }

        // Services rows.
        if services_visible || l.compact_services {
            let panel = if l.compact_services {
                l.table
            } else {
                l.services
            };
            let row_h = if l.compact_services {
                m.svc_row_h_narrow
            } else {
                m.svc_row_h
            };
            let svc_top = panel.y0 + if l.compact_services { 38.0 } else { 48.0 };
            for (i, def) in ServiceCatalog::ALL.iter().enumerate() {
                let y = svc_top + i as f64 * row_h;
                if y + row_h > panel.y1 - 6.0 {
                    break;
                }
                // Rows keep a token gap between their tiles: `S2` in the
                // docked column, `S1` in the dense compact route.
                let row = Rect::new(
                    panel.x0 + S3,
                    y,
                    panel.x1 - S3,
                    y + row_h - if l.compact_services { S1 } else { S2 },
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
        let body_top = l.table.y0 + self.metrics.table_head_h + 4.0;
        let row_h = if narrow {
            self.metrics.rule_row_h_narrow
        } else {
            self.metrics.rule_row_h
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
        let m = &self.metrics;
        let ctrl_h = m.ctrl_h;
        // Line 1.
        let y1 = l.form.y0 + m.form_title_h;
        // kind segmented Service|Port
        let kind_w = m.kind_w;
        for i in 0..2 {
            let x = fx0 + f64::from(i) * kind_w;
            l.kind_seg.push(Rect::new(x, y1, x + kind_w, y1 + ctrl_h));
        }
        let mut x = fx0 + kind_w * 2.0 + S3;
        if self.form.is_service {
            // service cycle selector: [<] name [>]. The hit rects stay
            // contiguous (no dead zone between a stepper and the value); the
            // value's painted well is inset `S1` from each stepper in
            // `draw_form_service_selector`, so the controls never touch.
            let sel_w = 188.0;
            let step_w = 30.0;
            l.service_prev = Rect::new(x, y1, x + step_w, y1 + ctrl_h);
            l.service_sel = Rect::new(x + step_w, y1, x + sel_w - step_w, y1 + ctrl_h);
            l.service_next = Rect::new(x + sel_w - step_w, y1, x + sel_w, y1 + ctrl_h);
            x += sel_w + S3;
        } else {
            // port field + proto segmented
            l.port_field = Rect::new(x, y1, x + 96.0, y1 + ctrl_h);
            x += 96.0 + S2;
            let pw = m.proto_w;
            for i in 0..2 {
                let px = x + f64::from(i) * pw;
                l.proto_seg.push(Rect::new(px, y1, px + pw, y1 + ctrl_h));
            }
            x += pw * 2.0 + S3;
        }
        let _ = x;
        // action segmented allow|deny — right aligned on line 1
        let act_w = m.act_w;
        for i in 0..2 {
            let ax = fx1 - act_w * 2.0 + f64::from(i) * act_w;
            l.action_seg
                .push(Rect::new(ax, y1, ax + act_w, y1 + ctrl_h));
        }

        // Line 2: source field + Add button, `S3` under line 1 (siblings).
        let y2 = y1 + ctrl_h + S3;
        let add_w = m.add_w;
        l.add_btn = Rect::new(fx1 - add_w, y2, fx1, y2 + ctrl_h);
        let src_x0 = fx0 + 64.0; // after "Source" label
        l.source_field = Rect::new(src_x0, y2, l.add_btn.x0 - S4, y2 + ctrl_h);
    }

    /// Stacked add-rule form for narrow windows: four short lines instead of a
    /// dense two-line row, so no control is crushed or clipped.
    fn build_form_narrow(&self, l: &mut Layout, fx0: f64, fx1: f64) {
        let m = &self.metrics;
        let ctrl_h = m.ctrl_h;
        // Line 1: kind segmented (Service|Port) on the left, action on the right.
        let y1 = l.form.y0 + m.form_title_h_narrow;
        let kind_w = m.kind_w - 2.0;
        for i in 0..2 {
            let x = fx0 + f64::from(i) * kind_w;
            l.kind_seg.push(Rect::new(x, y1, x + kind_w, y1 + ctrl_h));
        }
        let act_w = m.act_w - 2.0;
        for i in 0..2 {
            let ax = fx1 - act_w * 2.0 + f64::from(i) * act_w;
            l.action_seg
                .push(Rect::new(ax, y1, ax + act_w, y1 + ctrl_h));
        }

        // Line 2: service selector OR port + proto, spanning the width.
        let y2 = y1 + ctrl_h + 12.0;
        if self.form.is_service {
            let prev_w = 34.0;
            l.service_prev = Rect::new(fx0, y2, fx0 + prev_w, y2 + ctrl_h);
            l.service_next = Rect::new(fx1 - prev_w, y2, fx1, y2 + ctrl_h);
            // Contiguous hit rects; the painted well is inset `S1` (see the
            // wide layout above).
            l.service_sel = Rect::new(fx0 + prev_w, y2, fx1 - prev_w, y2 + ctrl_h);
        } else {
            let pw = m.proto_w;
            let port_x1 = fx1 - pw * 2.0 - 8.0;
            l.port_field = Rect::new(fx0, y2, port_x1, y2 + ctrl_h);
            for i in 0..2 {
                let px = fx1 - pw * 2.0 + f64::from(i) * pw;
                l.proto_seg.push(Rect::new(px, y2, px + pw, y2 + ctrl_h));
            }
        }

        // Line 3: source field (label sits above it), full width.
        let y3 = y2 + ctrl_h + m.form_source_gap;
        l.source_field = Rect::new(fx0, y3, fx1, y3 + ctrl_h);

        // Line 4: Add button, full width.
        let y4 = y3 + ctrl_h + 12.0;
        l.add_btn = Rect::new(fx0, y4, fx1, y4 + ctrl_h);
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

    /// Re-read the host after the user has started firewalld or fixed an
    /// authorization problem. Read-only until a valid configuration is
    /// found; recovery never seeds demo data.
    fn retry_connect(&self) {
        self.invoke(&FirewallEffect::Retry);
    }

    /// Show how reading firewalld ended, answering with the configuration to
    /// import. Every retry route shares this fail-closed behaviour; a failed
    /// read never changes the configuration.
    pub(crate) fn finish_retry(
        &mut self,
        read: Result<FirewallConfig, String>,
    ) -> Result<FirewallConfig, String> {
        match read {
            Ok(config) => {
                self.selected_zone = config.default_zone;
                self.pending_delete = None;
                // What this device's firewalld reported it already holds.
                if let Some(applied) = &self.applied
                    && self.config.zones.is_empty()
                {
                    crate::resource::remember_applied(applied, &config.canonical());
                }
                self.status = "Connected to firewalld and refreshed the live configuration".into();
                Ok(config)
            }
            Err(message) => {
                self.status.clone_from(&message);
                Err(message)
            }
        }
    }

    fn add_rule_from_form(&mut self) {
        // Nothing to append to if the selected zone isn't in the config.
        if self.config.zone(self.selected_zone).is_none() {
            return;
        }
        let matcher = self.form.matcher();
        let source = self.form.source.value().trim().to_string();
        let action = self.form.action;
        if let Matcher::Port { port, .. } = &matcher
            && let Err(message) = validate_port(port)
        {
            self.status = format!("Rule not added: {message}");
            return;
        }
        if let Err(message) = validate_source(&source) {
            self.status = format!("Rule not added: {message}");
            return;
        }
        let label = matcher.label();
        // Mint the rule (id and all) at authoring time, then dispatch it as the
        // action both the form and the palette build.
        let rule = Rule::new(matcher, action, source);
        let id = rule.id;
        self.dispatch(&crate::actions::add_rule_action(self.selected_zone, rule));
        self.selected_rule = Some(id);
        self.status = format!("Added {} {} rule", action.label(), label);
    }

    fn delete_rule(&mut self, id: RuleId) {
        self.dispatch(&crate::actions::delete_rule_action(self.selected_zone, id));
        if self.selected_rule == Some(id) {
            self.selected_rule = None;
        }
        self.status = "Rule deleted".to_string();
        self.pending_delete = None;
    }

    fn toggle_rule(&self, id: RuleId) {
        let on = self
            .config
            .zone(self.selected_zone)
            .and_then(|zone| zone.rules.iter().find(|rule| rule.id == id))
            .is_some_and(|rule| !rule.enabled);
        self.dispatch(&crate::actions::set_rule_enabled_action(
            self.selected_zone,
            id,
            on,
        ));
    }

    fn toggle_service(&mut self, idx: usize) {
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
        self.dispatch(&crate::actions::set_service_action(
            self.selected_zone,
            id,
            on,
        ));
        self.status = format!(
            "{} {} in {}",
            def.name,
            if on { "enabled" } else { "disabled" },
            self.selected_zone.name()
        );
    }

    fn set_active_zone(&mut self) {
        self.dispatch(&crate::actions::set_default_zone_action(self.selected_zone));
        self.status = format!("{} is now the active zone", self.selected_zone.name());
    }

    fn toggle_panic(&mut self) {
        let on = !self.config.panic_mode;
        self.dispatch(&crate::actions::set_panic_action(on));
        self.status = if on {
            "PANIC MODE ON — all traffic dropped".to_string()
        } else {
            "Panic mode off".to_string()
        };
    }

    fn toggle_enabled(&mut self) {
        let on = !self.config.enabled;
        self.dispatch(&crate::actions::set_enabled_action(on));
        self.status = if on {
            "Firewall enabled".to_string()
        } else {
            "Firewall stopped".to_string()
        };
    }

    /// Apply the stored configuration to this device now.
    fn apply(&self) {
        self.invoke(&FirewallEffect::ApplyConfig);
    }

    /// Show how an apply of `config` ended, and remember a successful one
    /// as what this device's firewalld holds.
    pub(crate) fn finish_apply(
        &mut self,
        config: &FirewallConfig,
        result: &crate::resource::Applied,
    ) {
        self.status = crate::resource::applied_status(result);
        if result.is_ok()
            && let Some(applied) = &self.applied
        {
            crate::resource::remember_applied(applied, config);
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
            &mut ui::widget::EventCtx {
                scale_factor,
                modifiers: &modifiers,
                state,
                path,
                handled: &mut handled,
                focus_target: &mut ft,
                node_id: NodeId::new(0),
                taffy,
            },
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

/// The face a selected row takes on a ground that is not the window tier:
/// `(fill, edge)`. `selection_tier` lifts `surfaces.base → raised`, and on
/// the light preset `raised` *is* the card the rules table and the overlay
/// rail are painted on, so the lift vanished there. The fill is the derived
/// edge colour of the ground used as a wash — `edge_on` climbs its alpha
/// ladder until the step registers on that ground, pure-black high-contrast
/// included — and the ring is the panel-strength derived edge: a step, never
/// an accent wash (HIG §9.3).
///
/// Open — needs coordinator: `ui::chrome::selection_tier_on(theme, state,
/// ground)` once merged.
fn selected_on(ground: Color) -> (Color, Color) {
    (
        ui::surface::edge_on(ground),
        ui::surface::edge_on_with(ground, PANEL_BORDER_ALPHA),
    )
}

impl Widget for FirewallApp {
    fn sync(&mut self, _state: &mut AppModel, _path: &ModelPath) {
        if let Some(status) = self.outcome.borrow_mut().take() {
            self.status = status;
        }
    }

    fn adjust_layout_with_measure(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        tr: &mut TextRenderer,
    ) -> bool {
        // The root node is percent-sized, so taffy has nothing to re-solve;
        // the measurement feeds `relayout`, which `apply_layout` runs next.
        let before = self.metrics.clone();
        self.measure_metrics(tr);
        before != self.metrics
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
        // out against the freshly-settled geometry so the drawer slides. The
        // bands are re-measured first so a zone change or text-scale change
        // is laid out and painted from the same numbers this frame.
        self.rail.tick(false);
        self.measure_metrics(tr);
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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                Self::forward_to_input(
                    &mut self.form.port,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
                );
                Self::forward_to_input(
                    &mut self.form.source,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
                );
            }
            WindowEvent::KeyboardInput { .. } => {
                self.handle_key(
                    event,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.handled,
                    ctx.taffy,
                );
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_click(event, ctx, self.last_cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                ..
            } => {
                Self::forward_to_input(
                    &mut self.form.port,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
                );
                Self::forward_to_input(
                    &mut self.form.source,
                    event,
                    ctx.scale_factor,
                    *ctx.modifiers,
                    ctx.state,
                    ctx.path,
                    ctx.taffy,
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

    #[allow(
        clippy::too_many_lines,
        reason = "the complete accessibility tree is intentionally visible together"
    )]
    fn accessibility(&self) -> Option<AccessNode> {
        let mut root = AccessNode::new(Role::Window)
            .with_name("Firewall")
            .with_bounds(self.rect);
        if !self.status.is_empty() {
            root.children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_hashed_key("firewall.status")
                    .with_live(Politeness::Polite),
            );
        }
        if !self.available {
            root.children.push(
                AccessNode::new(Role::Button)
                    .with_name("Retry connection to firewalld")
                    .with_description("Re-read the host firewall without changing rules")
                    .with_action_target("firewall.retry")
                    .with_bounds(self.layout.recover_btn)
                    .with_state(AccessState::new().focusable(true)),
            );
            return Some(root);
        }
        root.children.push(
            AccessNode::new(Role::Switch)
                .with_name("Firewall service")
                .with_value(if self.config.enabled {
                    "Active"
                } else {
                    "Stopped"
                })
                .with_action_target("firewall.header.enabled")
                .with_bounds(self.layout.enable_pill)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.config.enabled),
                ),
        );
        root.children.push(
            AccessNode::new(Role::ToggleButton)
                .with_name("Panic mode")
                .with_value(if self.config.panic_mode { "On" } else { "Off" })
                .with_action_target("firewall.header.panic")
                .with_bounds(self.layout.panic_btn)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .checked(self.config.panic_mode),
                ),
        );
        root.children.push(
            AccessNode::new(Role::Button)
                .with_name("Apply firewall configuration")
                .with_action_target("firewall.header.apply")
                .with_bounds(self.layout.apply_btn)
                .with_state(AccessState::new().focusable(true)),
        );
        let mut zones =
            VirtualListSemantics::new("Firewall zones", self.layout.rail, ZoneKind::ALL.len());
        for (index, (zone, bounds)) in self.layout.zone_rows.iter().enumerate() {
            zones.push(
                ListSemanticRow::new(
                    format!("firewall.zone:{}", zone.name()),
                    zone.name(),
                    *bounds,
                    index + 1,
                )
                .description(zone.description())
                .selected(*zone == self.selected_zone),
            );
        }
        root.children
            .push(zones.accessibility().with_hashed_key("firewall.zones"));
        if !self.layout.content_seg.is_empty() {
            let mut routes = AccessNode::new(Role::Group)
                .with_name("Firewall content")
                .with_description("Choose rules or services")
                .with_hashed_key("firewall.routes");
            for (index, bounds) in self.layout.content_seg.iter().enumerate() {
                let (name, route, selected) = if index == 0 {
                    ("Rules", "rules", self.compact_view == CompactView::Rules)
                } else {
                    (
                        "Services",
                        "services",
                        self.compact_view == CompactView::Services,
                    )
                };
                routes.children.push(
                    AccessNode::new(Role::ToggleButton)
                        .with_name(name)
                        .with_action_target(format!("firewall.route:{route}"))
                        .with_bounds(*bounds)
                        .with_state(AccessState::new().focusable(true).checked(selected)),
                );
            }
            root.children.push(routes);
        }
        if self.layout.compact_services {
            root.children.push(
                self.services_accessibility()
                    .with_hashed_key("firewall.services"),
            );
        } else {
            root.children.push(self.rules_table_accessibility());
            if self.layout.services_visible {
                root.children.push(
                    self.services_accessibility()
                        .with_hashed_key("firewall.services"),
                );
            }
        }
        Some(root)
    }

    fn needs_animation_frame(&self) -> bool {
        self.rail.is_animating()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &A11yAction,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if target == "firewall.retry" && matches!(action, A11yAction::Activate) {
            self.retry_connect();
            return true;
        }
        let toggles = matches!(action, A11yAction::Activate | A11yAction::ToggleCheck);
        match target {
            "firewall.header.enabled" if toggles => {
                self.toggle_enabled();
                self.set_form_focus(None);
                return true;
            }
            "firewall.header.panic" if toggles => {
                self.toggle_panic();
                self.set_form_focus(None);
                return true;
            }
            "firewall.header.apply" if matches!(action, A11yAction::Activate) => {
                self.apply();
                self.set_form_focus(None);
                return true;
            }
            _ => {}
        }
        if let Some(route) = target.strip_prefix("firewall.route:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
        {
            self.compact_view = match route {
                "rules" => CompactView::Rules,
                "services" => CompactView::Services,
                _ => return false,
            };
            self.set_form_focus(None);
            self.relayout();
            return true;
        }
        if let Some(target) = target.strip_prefix("firewall.service.toggle:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
            && let Some((zone, service)) = target.split_once(':')
            && ZoneKind::from_name(zone) == Some(self.selected_zone)
            && let Some(index) = ServiceCatalog::ALL
                .iter()
                .position(|candidate| candidate.id == service)
        {
            self.toggle_service(index);
            self.relayout();
            return true;
        }
        if let Some(name) = target.strip_prefix("firewall.zone:")
            && matches!(action, A11yAction::Activate | A11yAction::Focus)
            && let Some(zone) = ZoneKind::from_name(name)
        {
            self.select_zone(zone);
            self.relayout();
            return true;
        }
        if let Some(id) = target.strip_prefix("firewall.rule.select:")
            && matches!(action, A11yAction::Activate | A11yAction::Focus)
            && let Ok(id) = RuleId::parse_str(id)
            && self
                .selected_zone_ref()
                .is_some_and(|zone| zone.rules.iter().any(|rule| rule.id == id))
        {
            self.selected_rule = Some(id);
            self.relayout();
            return true;
        }
        if let Some(id) = target.strip_prefix("firewall.rule.toggle:")
            && matches!(action, A11yAction::Activate | A11yAction::ToggleCheck)
            && let Ok(id) = RuleId::parse_str(id)
            && self
                .selected_zone_ref()
                .is_some_and(|zone| zone.rules.iter().any(|rule| rule.id == id))
        {
            self.toggle_rule(id);
            return true;
        }
        if let Some(id) = target.strip_prefix("firewall.rule.delete:")
            && matches!(action, A11yAction::Activate)
            && let Ok(id) = RuleId::parse_str(id)
            && self
                .selected_zone_ref()
                .is_some_and(|zone| zone.rules.iter().any(|rule| rule.id == id))
        {
            if self.pending_delete == Some(id) {
                self.delete_rule(id);
            } else {
                self.pending_delete = Some(id);
                self.status =
                    "Delete rule? Activate its delete button again to confirm (Esc cancels)".into();
                self.relayout();
            }
            return true;
        }
        false
    }
}

impl FirewallApp {
    fn services_accessibility(&self) -> AccessNode {
        let zone = self.config.zone(self.selected_zone);
        let panel = if self.layout.compact_services {
            self.layout.table
        } else {
            self.layout.services
        };
        let mut services = VirtualListSemantics::new(
            format!("Services in {}", self.selected_zone.name()),
            panel,
            ServiceCatalog::ALL.len(),
        )
        .description("Network services allowed through this firewall zone");
        for (index, row, toggle) in &self.layout.service_rows {
            let Some(service) = ServiceCatalog::ALL.get(*index) else {
                continue;
            };
            let enabled = zone.is_some_and(|zone| zone.has_service(service.id));
            let target = format!(
                "firewall.service.toggle:{}:{}",
                self.selected_zone.name(),
                service.id
            );
            services.push(
                ListSemanticRow::static_row(
                    format!(
                        "firewall.service:{}:{}",
                        self.selected_zone.name(),
                        service.id
                    ),
                    service.name,
                    *row,
                    index + 1,
                )
                .description(format!(
                    "{}; {}/{}",
                    service.description, service.ports, service.proto
                ))
                .children([AccessNode::new(Role::Switch)
                    .with_name(format!("Allow {}", service.name))
                    .with_action_target(target)
                    .with_bounds(*toggle)
                    .with_state(AccessState::new().focusable(true).checked(enabled))]),
            );
        }
        services.accessibility()
    }

    fn rules_table_accessibility(&self) -> AccessNode {
        let table_name = format!("Rules in {}", self.selected_zone.name());
        let RuleColumns {
            text: target,
            proto,
            action,
            source,
            ctrl: controls,
            ..
        } = rule_columns(self.layout.table);
        let right = self.layout.table.x1 - 12.0;
        let header = self.layout.table.y0..self.layout.table.y0 + 30.0;
        let columns = [
            ("Target", target, proto),
            ("Protocol", proto, action),
            ("Action", action, source),
            ("Source", source, controls),
            ("Status", controls, right),
        ]
        .into_iter()
        .map(|(name, x0, x1)| {
            TableSemanticColumn::new(name, Some(Rect::new(x0, header.start, x1, header.end)))
        })
        .collect::<Vec<_>>();
        let filtered = self.filtered_indices();
        let total = filtered.len();
        let rows = self
            .layout
            .rule_rows
            .iter()
            .filter_map(|row| {
                let zone = self.selected_zone_ref()?;
                let rule = zone.rules.iter().find(|rule| rule.id == row.id)?;
                let index = filtered.iter().position(|&source_index| {
                    zone.rules.get(source_index).is_some_and(|r| r.id == row.id)
                })?;
                let values = [
                    rule.matcher.label(),
                    rule.matcher.proto_label(),
                    rule.action.label().to_owned(),
                    rule.source_label().to_owned(),
                    if rule.enabled { "On" } else { "Off" }.to_owned(),
                ];
                let bounds = [
                    (target, proto),
                    (proto, action),
                    (action, source),
                    (source, controls),
                    (controls, right),
                ];
                let cells = values
                    .into_iter()
                    .zip(bounds)
                    .map(|(value, (x0, x1))| {
                        TableSemanticCell::new(
                            Some(value),
                            Some(Rect::new(x0, row.rect.y0, x1, row.rect.y1)),
                        )
                    })
                    .collect();
                Some(
                    TableSemanticRow::new(index, total, Some(row.rect), cells)
                        .selected(self.selected_rule == Some(row.id))
                        .action_target(format!("firewall.rule.select:{}", row.id))
                        .controls(vec![
                            TableSemanticControl::toggle(
                                format!(
                                    "{} {} rule",
                                    if rule.enabled { "Disable" } else { "Enable" },
                                    rule.matcher.label()
                                ),
                                format!("firewall.rule.toggle:{}", row.id),
                                row.toggle,
                                rule.enabled,
                            ),
                            TableSemanticControl::button(
                                if self.pending_delete == Some(row.id) {
                                    format!("Confirm deletion of {} rule", rule.matcher.label())
                                } else {
                                    format!("Delete {} rule", rule.matcher.label())
                                },
                                format!("firewall.rule.delete:{}", row.id),
                                row.delete,
                            ),
                        ]),
                )
            })
            .collect();
        VirtualTableSemantics::new(&table_name, self.layout.table)
            .description(format!("{total} filtered firewall rules"))
            .read_only(false)
            .hashed_key(("firewall-rules", self.selected_zone.name()))
            .columns(columns)
            .rows(rows)
            .build()
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one keyboard dispatch table covering every focusable region"
    )]
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
        if !self.available
            && matches!(&key_event.logical_key, Key::Character(key) if key.eq_ignore_ascii_case("r"))
        {
            self.retry_connect();
            *handled = true;
            return;
        }
        // Global keys first.
        match &key_event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.set_form_focus(None);
                if self.pending_delete.take().is_some() {
                    self.status = "Delete cancelled".to_string();
                }
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
                self.add_rule_from_form();
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
                    self.delete_rule(id);
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
                    self.toggle_panic();
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
                } else if s.eq_ignore_ascii_case("s") && !self.layout.services_visible {
                    self.compact_view = match self.compact_view {
                        CompactView::Rules => CompactView::Services,
                        CompactView::Services => CompactView::Rules,
                    };
                    self.status = match self.compact_view {
                        CompactView::Rules => "Showing rules (press S for services)".into(),
                        CompactView::Services => "Showing services (press S for rules)".into(),
                    };
                    *handled = true;
                }
            }
            _ => {}
        }
    }

    fn handle_click(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>, p: Point) {
        // Nothing is interactive while firewalld is unavailable — there is
        // no real config to edit or apply.
        if !self.available {
            if self.layout.recover_btn.contains(p) {
                self.retry_connect();
                *ctx.handled = true;
            }
            return;
        }

        let l = self.layout.clone();

        if self.handle_click_rail(p, &l, ctx.handled) {
            return;
        }
        if self.handle_click_primary(p, &l, ctx.handled) {
            return;
        }
        if self.handle_click_form(p, &l, ctx.handled) {
            return;
        }
        if self.handle_click_text_field(event, ctx, p, &l) {
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
    fn handle_click_primary(&mut self, p: Point, l: &Layout, handled: &mut bool) -> bool {
        // Header controls.
        if l.enable_pill.contains(p) {
            self.toggle_enabled();
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        if l.panic_btn.contains(p) {
            self.toggle_panic();
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
            self.set_active_zone();
            *handled = true;
            self.set_form_focus(None);
            return true;
        }

        // Compact Rules/Services route. It is local presentation state, so a
        // narrow window never removes service management from the app.
        if let Some(index) = seg_index(&l.content_seg, p) {
            self.compact_view = if index == 0 {
                CompactView::Rules
            } else {
                CompactView::Services
            };
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
                if self.pending_delete == Some(row.id) {
                    self.delete_rule(row.id);
                } else {
                    self.pending_delete = Some(row.id);
                    self.status =
                        "Delete rule? Click its trash button again to confirm (Esc cancels)".into();
                }
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
            if row.toggle.contains(p) {
                self.toggle_rule(row.id);
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
                self.toggle_service(*idx);
                *handled = true;
                self.set_form_focus(None);
                return true;
            }
        }

        false
    }

    /// Add-rule form controls: kind/proto/action segmented, service
    /// prev/next, add button. Returns whether the click was consumed here.
    fn handle_click_form(&mut self, p: Point, l: &Layout, handled: &mut bool) -> bool {
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
            self.add_rule_from_form();
            *handled = true;
            self.set_form_focus(None);
            return true;
        }
        false
    }

    /// Port/source text fields: focus + forward the press for caret
    /// placement. Returns whether the click was consumed here.
    fn handle_click_text_field(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        p: Point,
        l: &Layout,
    ) -> bool {
        if !self.form.is_service && l.port_field.contains(p) {
            self.set_form_focus(Some(FocusField::Port));
            Self::forward_to_input(
                &mut self.form.port,
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.taffy,
            );
            *ctx.handled = true;
            return true;
        }
        if l.source_field.contains(p) {
            self.set_form_focus(Some(FocusField::Source));
            Self::forward_to_input(
                &mut self.form.source,
                event,
                ctx.scale_factor,
                *ctx.modifiers,
                ctx.state,
                ctx.path,
                ctx.taffy,
            );
            *ctx.handled = true;
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
        if l.compact_services {
            self.draw_services(scene, t, tr, theme, &l);
        } else {
            self.draw_table(scene, t, tr, theme, &l);
            if l.services_visible {
                self.draw_services(scene, t, tr, theme, &l);
            }
            self.draw_form(scene, t, tr, theme, presence, &l);
        }

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
        let ch = 220.0;
        let cx = area.x0 + (area.width() - cw) / 2.0;
        let cy = area.y0 + (area.height() - ch) / 2.0;
        let card = Rect::new(cx, cy, cx + cw, cy + ch);
        paint::card(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            "firewalld unavailable",
            18.0,
            theme.text_primary,
            Point::new(card.x0 + 24.0, card.y0 + 26.0),
            cw - 48.0,
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            if self.status.is_empty() {
                "The firewalld service is not installed or not running."
            } else {
                &self.status
            },
            13.0,
            if self.status.is_empty() {
                theme.text_dim
            } else {
                theme.tokens.colors.semantic.danger
            },
            Point::new(card.x0 + 24.0, card.y0 + 62.0),
            cw - 48.0,
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            "No live firewall is displayed, and no rules have been changed.",
            13.0,
            theme.text_dim,
            Point::new(card.x0 + 24.0, card.y0 + 106.0),
            cw - 48.0,
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            "Start or authorize firewalld, then retry (keyboard: R).",
            12.0,
            theme.text_dim,
            Point::new(card.x0 + 24.0, card.y0 + 132.0),
            cw - 48.0,
        );
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(l.recover_btn, "Retry connection")
                .primary()
                .radius(8.0)
                .text_size(12.0)
                .hovered(l.recover_btn.contains(self.last_cursor)),
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
        // Bar role: one raised band with a derived hairline under it.
        let h = l.header;
        paint::fill_rect(scene, t, h, theme.tokens.colors.surfaces.raised);
        paint::hline(
            scene,
            t,
            h.x0,
            h.x1,
            h.y1 - 0.5,
            chrome_hairline(theme),
            1.0,
        );

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
        // When shown, shift it right past the header hamburger and elide it
        // under the controls: one line, centred on the controls' own centre
        // line (a wrapped second line used to paint across the zone rail).
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
            let shown = tr.elide_to_width(&sub, 13.0, sub_max as f32);
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                &shown,
                13.0,
                sub_color,
                Rect {
                    x0: sub_x,
                    x1: sub_x + sub_max,
                    ..h
                },
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
            let mode_txt = match self.mode {
                Mode::Live => "live",
                Mode::DryRun => "dry-run",
            };
            paint::label_right_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                mode_txt,
                11.0,
                theme.text_dim,
                Rect {
                    x1: l.enable_pill.x0 - 10.0,
                    ..h
                },
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
        // A status-tinted pill: the tint is its contrast step and the edge
        // is the same semantic colour, so it reads on both palettes.
        ui::surface::pill(
            scene,
            t,
            l.enable_pill,
            pill_bg,
            Some(pill_fg.with_alpha_factor(0.55)),
            None,
        );
        let w = paint::text_width(tr, pill_txt, 12.0);
        paint::label_v(
            &mut PaintCtx::new(scene, t, tr, theme),
            pill_txt,
            12.0,
            pill_fg,
            Rect {
                x0: l.enable_pill.x0 + (l.enable_pill.width() - w) / 2.0,
                x1: l.enable_pill.x0 + (l.enable_pill.width() - w) / 2.0 + w + 2.0,
                ..l.enable_pill
            },
        );

        // Panic and Apply are the shared button face: rest edge, hover and
        // ink contrast come with it.
        let pt = if self.config.panic_mode {
            "Panic ON"
        } else {
            "Panic"
        };
        let panic = ButtonFace::new(l.panic_btn, pt)
            .radius(l.panic_btn.height() / 2.0)
            .text_size(12.0)
            .hovered(l.panic_btn.contains(self.last_cursor));
        let panic = if self.config.panic_mode {
            panic.danger()
        } else {
            panic
        };
        Button::paint(scene, t, tr, theme, &panic);
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(l.apply_btn, "Apply")
                .primary()
                .radius(l.apply_btn.height() / 2.0)
                .text_size(13.0)
                .hovered(l.apply_btn.contains(self.last_cursor)),
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
        // real Panel-role surface when the drawer floats over content in
        // overlay mode. The right edge and the selected row's face are both
        // derived against whatever the rail actually sits on.
        let (divider, selected_face) = if self.rail.is_overlay() {
            let ground = panel_ground(theme);
            paint::fill_rect(scene, t, rail, ground);
            let (fill, edge) = selected_on(ground);
            (panel_border(theme), (fill, Some(edge)))
        } else {
            // On the window tier the shared tier is the answer (HIG §9.3).
            let tier = selection_tier(theme, SelectionState::active(true));
            (
                ui::surface::edge_on(theme.tokens.colors.surfaces.base),
                (tier.fill, tier.border),
            )
        };
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            divider,
            None,
            &vello::kurbo::Line::new(
                Point::new(rail.x1 - 0.5, rail.y0),
                Point::new(rail.x1 - 0.5, rail.y1),
            ),
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            "ZONES",
            11.0,
            theme.text_dim,
            Point::new(rail.x0 + 18.0, rail.y0 + 18.0),
            120.0,
        );

        for (kind, rect) in &l.zone_rows {
            self.draw_rail_zone_row(scene, t, tr, theme, *kind, *rect, selected_face);
        }
        scene.pop_layer();
    }

    /// One zone row of the rail: the selected face (a lift with a derived
    /// edge, HIG §9.3 — the accent stays for the *live* default zone's chip),
    /// the trust dot, the name and the active chip or rule count. Split out
    /// of [`Self::draw_rail`] to keep it under the line budget.
    fn draw_rail_zone_row(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        kind: ZoneKind,
        rect: Rect,
        selected_face: (Color, Option<Color>),
    ) {
        let selected = kind == self.selected_zone;
        let is_default = kind == self.config.default_zone;
        if selected {
            let (fill, edge) = selected_face;
            paint::fill_round(scene, t, rect, 8.0, fill);
            if let Some(edge) = edge {
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.0),
                    t,
                    edge,
                    None,
                    &ui::surface::edge_outline(rect, 8.0),
                );
            }
        }
        // Trust dot.
        let cy = f64::midpoint(rect.y0, rect.y1);
        paint::dot(
            scene,
            t,
            Point::new(rect.x0 + S4, cy),
            4.0,
            trust_color(theme, kind.trust()),
        );
        // Name.
        let name_color = if selected {
            theme.text_primary
        } else {
            theme.text_dim
        };
        paint::label_v(
            &mut PaintCtx::new(scene, t, tr, theme),
            kind.name(),
            14.0,
            name_color,
            Rect {
                x0: rect.x0 + 30.0,
                x1: rect.x0 + 30.0 + 120.0,
                ..rect
            },
        );

        // Active marker chip.
        if is_default {
            let _ = paint::chip(
                &mut PaintCtx::new(scene, t, tr, theme),
                "active",
                &paint::ChipStyle {
                    size: 9.0,
                    ink: theme.tokens.colors.on_surface(theme.accent_primary).ink,
                    fill: theme.accent_primary,
                    pad_x: 6.0,
                    height: 16.0,
                    radius: 8.0,
                },
                Point::new(rect.x1 - 52.0, cy),
            );
        } else if let Some(z) = self.config.zone(kind) {
            // Rule + service count.
            let n = z.rules.len() + z.services.len();
            if n > 0 {
                paint::label_right_v(
                    &mut PaintCtx::new(scene, t, tr, theme),
                    &n.to_string(),
                    11.0,
                    theme.text_dim,
                    Rect {
                        x1: rect.x1 - 8.0,
                        ..rect
                    },
                );
            }
        }
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
        paint::card(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);
        if self.config.panic_mode {
            // Panic is live state: a semantic ring on the card's own edge
            // outline, so it replaces the rest edge instead of straddling it.
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                t,
                theme.tokens.colors.semantic.danger,
                None,
                &ui::surface::edge_outline(card, CARD_R),
            );
        }

        let Some(z) = self.config.zone(self.selected_zone) else {
            return;
        };
        let inset_x = card.x0 + PAD;
        self.draw_summary_title(scene, t, tr, theme, card, inset_x);
        Self::draw_summary_chips(scene, t, tr, theme, l, z);
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
            &mut PaintCtx::new(scene, t, tr, theme),
            self.selected_zone.name(),
            22.0,
            theme.text_primary,
            Point::new(inset_x, card.y0 + PAD),
            260.0,
        );
        let (title_w, title_h) = tr.measure(self.selected_zone.name(), 22.0, None);
        if self.selected_zone == self.config.default_zone {
            // The chip rides the title's measured centre line rather than a
            // nominal offset, so it stays beside the name at every text scale.
            let _ = paint::chip(
                &mut PaintCtx::new(scene, t, tr, theme),
                "ACTIVE ZONE",
                &paint::ChipStyle {
                    size: 9.0,
                    ink: theme.tokens.colors.on_surface(theme.accent_primary).ink,
                    fill: theme.accent_primary,
                    pad_x: 7.0,
                    height: 16.0,
                    radius: 8.0,
                },
                Point::new(
                    inset_x + f64::from(title_w) + S3,
                    card.y0 + PAD + f64::from(title_h) / 2.0,
                ),
            );
        }
        // The description follows the title's measured line, the chips
        // follow the description (see `Metrics::desc_top` / `chips_top`).
        let shown = tr.elide_to_width(
            self.selected_zone.description(),
            12.0,
            (card.width() - 2.0 * PAD) as f32,
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            &shown,
            12.0,
            theme.text_dim,
            Point::new(inset_x, card.y0 + self.metrics.desc_top()),
            card.width() - 2.0 * PAD,
        );
    }

    /// Stat chips, painted into the rects the layout wrapped them into (so a
    /// narrow card never runs them off its right edge and the card is exactly
    /// as tall as the rows need).
    fn draw_summary_chips(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
        z: &Zone,
    ) {
        for ((caption, value, tone), rect) in stat_chip_specs(z).iter().zip(&l.stat_chips) {
            let color = match tone {
                ChipTone::Target(target) => target_color(theme, *target),
                ChipTone::Primary => theme.text_primary,
                ChipTone::Success => theme.tokens.colors.semantic.success,
                ChipTone::Danger => theme.tokens.colors.semantic.danger,
                ChipTone::Info => theme.tokens.colors.semantic.info,
            };
            Self::stat_chip(scene, t, tr, theme, caption, value, color, *rect);
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
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(l.set_active_btn, "Set as active")
                .radius(8.0)
                .text_size(12.0)
                .hovered(l.set_active_btn.contains(self.last_cursor)),
        );
    }

    /// A two-tier stat: a dim caption with a coloured value beneath, framed
    /// in a soft chip filling `rect` (sized by `Metrics::measure` from the
    /// same caption/value).
    fn stat_chip(
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        caption: &str,
        value: &str,
        value_color: Color,
        rect: Rect,
    ) {
        let (_, cap_h) = tr.measure(caption, 9.0, None);
        let (_, val_h) = tr.measure(value, 13.0, None);
        let inner = rect.width() - 20.0;
        // A tile inset into the raised card: the window tier as its fill,
        // with the edge derived from it, so it reads on both palettes.
        paint::card(scene, t, rect, 8.0, theme.tokens.colors.surfaces.base);
        // Centre the caption/value stack in the chip so the top and bottom
        // paddings match instead of the value crowding the lower edge.
        let stack_h = f64::from(cap_h) + f64::from(val_h);
        let cap_top = rect.y0 + (rect.height() - stack_h) / 2.0;
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            caption,
            9.0,
            theme.text_dim,
            Point::new(rect.x0 + 10.0, cap_top),
            inner + 2.0,
        );
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            value,
            13.0,
            value_color,
            Point::new(rect.x0 + 10.0, cap_top + f64::from(cap_h)),
            inner + 2.0,
        );
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
        if !l.content_seg.is_empty() {
            let labels = ["Rules", "Services"];
            let selected = usize::from(l.compact_services);
            Self::draw_segmented(scene, t, tr, theme, &l.content_seg, &labels, selected);
            if l.compact_services {
                return;
            }
        }
        // The section caption is only painted when nothing else names the
        // section: with the Rules/Services route visible its selected segment
        // already reads "Rules", and a second copy collided with the filter
        // control at phone widths.
        if l.content_seg.is_empty() {
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "Rules",
                15.0,
                theme.text_primary,
                Rect::new(
                    l.summary.x0,
                    y0,
                    l.summary.x0 + 120.0,
                    y0 + first_seg.height(),
                ),
            );
        }
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
        paint::card(scene, t, table, CARD_R, theme.tokens.colors.surfaces.raised);

        let cols = rule_columns(table);
        let head_h = self.metrics.table_head_h;
        Self::draw_table_header(scene, t, tr, theme, l, table, cols, head_h);

        if l.rule_rows.is_empty() {
            paint::label(
                &mut PaintCtx::new(scene, t, tr, theme),
                "No rules match this filter.",
                13.0,
                theme.text_dim,
                Point::new(cols.text, table.y0 + head_h + PAD),
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
            self.draw_table_row(&mut PaintCtx::new(scene, t, tr, theme), l, cols, row, rule);
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
        cols: RuleColumns,
        head_h: f64,
    ) {
        // Captions sit in the caption band above the separator (30px at 1×,
        // taller once the type is), centred on the same measured line the
        // rows use, and start at the row text column (not at the status dot)
        // so they line up with what they label.
        let hy0 = table.y0;
        let hh = head_h;
        let hc = theme.text_dim;
        if l.narrow {
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "RULE",
                10.0,
                hc,
                Rect::new(cols.text, hy0, cols.text + 200.0, hy0 + hh),
            );
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "ON",
                10.0,
                hc,
                Rect::new(cols.ctrl, hy0, cols.ctrl + 60.0, hy0 + hh),
            );
        } else {
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "TARGET",
                10.0,
                hc,
                Rect::new(cols.text, hy0, cols.text + 200.0, hy0 + hh),
            );
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "PROTO",
                10.0,
                hc,
                Rect::new(cols.proto, hy0, cols.proto + 80.0, hy0 + hh),
            );
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "ACTION",
                10.0,
                hc,
                Rect::new(cols.action, hy0, cols.action + 80.0, hy0 + hh),
            );
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "SOURCE",
                10.0,
                hc,
                Rect::new(cols.source, hy0, cols.source + 160.0, hy0 + hh),
            );
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "ON",
                10.0,
                hc,
                Rect::new(cols.ctrl, hy0, cols.ctrl + 60.0, hy0 + hh),
            );
        }
        // The caption rule is derived against the table it sits in and inset
        // `S3` from the card edge (HIG §9.5), never run into the radius.
        paint::separator(
            scene,
            t,
            table.x0,
            table.x1,
            table.y0 + head_h,
            S3,
            theme.tokens.colors.surfaces.raised,
        );
    }

    /// One rule row: selection highlight, narrow/wide content, then the
    /// shared toggle + delete controls and row separator.
    fn draw_table_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        l: &Layout,
        cols: RuleColumns,
        row: &RuleRow,
        rule: &Rule,
    ) {
        let theme = ctx.theme;
        let selected = self.selected_rule == Some(row.id);
        if selected {
            // Selected row: a step measured against the raised table card it
            // sits on, with a derived ring — not an accent ring, and not
            // `selection_tier`'s lift, which is the card's own colour on light.
            let (fill, edge) = selected_on(theme.tokens.colors.surfaces.raised);
            paint::fill_round(ctx.scene, ctx.transform, row.rect, 8.0, fill);
            ctx.scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                ctx.transform,
                edge,
                None,
                &ui::surface::edge_outline(row.rect, 8.0),
            );
        }
        let style = RowStyle::for_rule(theme, rule);
        if l.narrow {
            self.draw_table_row_narrow(ctx, cols, row, rule, &style);
        } else {
            Self::draw_table_row_wide(ctx, cols, row, rule, &style);
        }

        Self::draw_table_row_controls(
            ctx.scene,
            ctx.transform,
            theme,
            row,
            rule.enabled,
            self.pending_delete == Some(row.id),
        );
    }

    /// Two-line row: target + action on top, proto · source beneath. Used
    /// when the table is too narrow for five columns abreast.
    fn draw_table_row_narrow(
        &self,
        ctx: &mut PaintCtx<'_>,
        cols: RuleColumns,
        row: &RuleRow,
        rule: &Rule,
        style: &RowStyle,
    ) {
        let (m, src) = (&self.metrics, rule.source_label());
        // First-line centre and second-line top follow the measured 14px
        // label, as the row height the layout built does.
        let dot_y = row.rect.y0 + m.rule_dot_y;
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(cols.dot, dot_y),
            3.0,
            if style.dim {
                alpha(style.dot_color, 120)
            } else {
                style.dot_color
            },
        );
        // Action chip on the top line, right-aligned 8px before the toggle
        // (a fixed left edge ran under the toggle once the label doubled);
        // the target label elides against the chip.
        let chip_w = paint::text_width(ctx.text, style.action_text, 11.0) + 16.0;
        let chip_x = row.toggle.x0 - 8.0 - chip_w;
        let label_max = chip_x - cols.text - 8.0;
        paint::label(
            ctx,
            &rule.matcher.label(),
            14.0,
            style.primary,
            Point::new(cols.text, row.rect.y0 + 10.0),
            label_max.max(40.0),
        );
        let _ = paint::chip(
            ctx,
            style.action_text,
            &paint::ChipStyle {
                size: 11.0,
                ink: style.action_color,
                fill: alpha(style.action_color, 40),
                pad_x: 8.0,
                height: 20.0,
                radius: 6.0,
            },
            Point::new(chip_x, dot_y),
        );
        // Second line: proto · source.
        let sub = if src == "anywhere" {
            format!("{} · anywhere", rule.matcher.proto_label())
        } else {
            format!("{} · {}", rule.matcher.proto_label(), src)
        };
        paint::label(
            ctx,
            &sub,
            12.0,
            style.secondary,
            Point::new(cols.text, row.rect.y0 + m.rule_sub_top),
            row.rect.width() - 30.0,
        );
    }

    /// Five columns abreast: target, proto, action chip, source.
    fn draw_table_row_wide(
        ctx: &mut PaintCtx<'_>,
        cols: RuleColumns,
        row: &RuleRow,
        rule: &Rule,
        style: &RowStyle,
    ) {
        let src = rule.source_label();
        let RuleColumns {
            dot: cx_dot,
            text: cx_text,
            proto: cx_proto,
            action: cx_action,
            source: cx_source,
            ctrl: cx_ctrl,
        } = cols;
        let cy = f64::midpoint(row.rect.y0, row.rect.y1);
        paint::dot(
            ctx.scene,
            ctx.transform,
            Point::new(cx_dot, cy),
            3.0,
            if style.dim {
                alpha(style.dot_color, 120)
            } else {
                style.dot_color
            },
        );
        paint::label_v(
            ctx,
            &rule.matcher.label(),
            14.0,
            style.primary,
            Rect {
                x0: cx_text,
                x1: cx_proto - 4.0,
                ..row.rect
            },
        );
        paint::label_v(
            ctx,
            &rule.matcher.proto_label(),
            12.0,
            style.secondary,
            Rect {
                x0: cx_proto,
                x1: cx_proto + 60.0,
                ..row.rect
            },
        );
        let _ = paint::chip(
            ctx,
            style.action_text,
            &paint::ChipStyle {
                size: 11.0,
                ink: style.action_color,
                fill: alpha(style.action_color, 40),
                pad_x: 8.0,
                height: 20.0,
                radius: 6.0,
            },
            Point::new(cx_action, cy),
        );
        let src_color = if src == "anywhere" {
            style.secondary
        } else {
            style.primary
        };
        paint::label_v(
            ctx,
            src,
            12.0,
            src_color,
            Rect {
                x0: cx_source,
                x1: cx_ctrl - 8.0,
                ..row.rect
            },
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
        delete_armed: bool,
    ) {
        draw_toggle(scene, t, theme, row.toggle, enabled);

        // Delete (×) drawn as two strokes inside a soft danger button.
        let d = row.delete;
        paint::fill_round(
            scene,
            t,
            d,
            6.0,
            alpha(
                theme.tokens.colors.semantic.danger,
                if delete_armed { 92 } else { 30 },
            ),
        );
        if delete_armed {
            paint::stroke_round(scene, t, d, 6.0, theme.tokens.colors.semantic.danger, 1.0);
        }
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

        // Row separator: centred in the `S1` gap under the row, derived
        // against the table it sits in (the rows are already inset from the
        // card edge, so no further inset).
        paint::separator(
            scene,
            t,
            row.rect.x0,
            row.rect.x1,
            row.rect.y1 + S1 / 2.0,
            0.0,
            theme.tokens.colors.surfaces.raised,
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
        let panel = if l.compact_services {
            l.table
        } else {
            l.services
        };
        // A docked side pane on the window tier: its left edge is derived
        // against that ground, and each enabled service is a raised tile
        // with its own derived edge (an "on" state that reads on both
        // palettes, where a bare lift did not).
        paint::fill_rect(scene, t, panel, theme.tokens.colors.surfaces.base);
        scene.stroke(
            &vello::kurbo::Stroke::new(1.0),
            t,
            ui::surface::edge_on(theme.tokens.colors.surfaces.base),
            None,
            &vello::kurbo::Line::new(
                Point::new(panel.x0 + 0.5, panel.y0),
                Point::new(panel.x0 + 0.5, panel.y1),
            ),
        );
        let on_count = self
            .config
            .zone(self.selected_zone)
            .map_or(0, |z| z.services.len());
        // Caption and count share one centre line inside the header band
        // above the first row (48px docked, 38px in the compact route; the
        // same offsets `layout_center` uses for `svc_top`).
        let header_h = if l.compact_services { 38.0 } else { 48.0 };
        paint::label_v(
            &mut PaintCtx::new(scene, t, tr, theme),
            "SERVICES",
            11.0,
            theme.text_dim,
            Rect::new(
                panel.x0 + 16.0,
                panel.y0,
                panel.x0 + 16.0 + 120.0,
                panel.y0 + header_h,
            ),
        );
        paint::label_right_v(
            &mut PaintCtx::new(scene, t, tr, theme),
            &format!("{on_count} enabled"),
            11.0,
            theme.accent_primary,
            Rect::new(panel.x0, panel.y0, panel.x1 - 16.0, panel.y0 + header_h),
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
                paint::card(scene, t, *row, 8.0, theme.tokens.colors.surfaces.raised);
            }
            let compact = l.compact_services;
            paint::label(
                &mut PaintCtx::new(scene, t, tr, theme),
                def.name,
                if compact { 13.0 } else { 14.0 },
                theme.text_primary,
                Point::new(row.x0 + 10.0, row.y0 + if compact { 3.0 } else { 5.0 }),
                row.width() - 60.0,
            );
            let detail = format!("{}/{}", def.ports, def.proto);
            paint::label(
                &mut PaintCtx::new(scene, t, tr, theme),
                &detail,
                10.0,
                theme.text_dim,
                Point::new(
                    if compact {
                        row.x0 + 118.0
                    } else {
                        row.x0 + 10.0
                    },
                    row.y0
                        + if compact {
                            5.0
                        } else {
                            self.metrics.svc_detail_top
                        },
                ),
                if compact {
                    row.width() - 180.0
                } else {
                    row.width() - 60.0
                },
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
        paint::card(scene, t, card, CARD_R, theme.tokens.colors.surfaces.raised);

        let title = format!("Add rule to {}", self.selected_zone.name());
        paint::label(
            &mut PaintCtx::new(scene, t, tr, theme),
            &title,
            13.0,
            theme.accent_primary,
            Point::new(card.x0 + PAD, card.y0 + S3),
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
        self.draw_form_add_button(scene, t, tr, theme, l);

        // Status is a polite, persistent acknowledgement rather than a transient
        // toast: keyboard and touch users can read validation and confirmation
        // feedback before acting again.
        if !self.status.is_empty() {
            let status_color = if self.status.starts_with("Rule not added")
                || self.status.starts_with("Delete rule?")
                || self.status.starts_with("Apply failed")
            {
                theme.tokens.colors.semantic.danger
            } else {
                theme.text_dim
            };
            // Stacked: the line under the title, inside the title band the
            // layout reserved for both. Wide: centred in the status band
            // under the last control row.
            let m = &self.metrics;
            let status_y = if l.narrow {
                card.y0 + 12.0 + f64::from(tr.measure("Hg", 13.0, None).1) + 2.0
            } else {
                paint::centered_top(
                    tr,
                    &self.status,
                    11.0,
                    card.y1 - m.form_status_h,
                    m.form_status_h,
                )
            };
            let shown = tr.elide_to_width(&self.status, 11.0, (card.width() - 32.0) as f32);
            paint::label(
                &mut PaintCtx::new(scene, t, tr, theme),
                &shown,
                11.0,
                status_color,
                Point::new(card.x0 + 16.0, status_y),
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
        // The two steppers are shared button faces; the value between them
        // is a field-like well on the window tier with its derived edge.
        for (rect, glyph) in [(l.service_prev, "<"), (l.service_next, ">")] {
            Button::paint(
                scene,
                t,
                tr,
                theme,
                &ButtonFace::new(rect, glyph)
                    .radius(7.0)
                    .text_size(14.0)
                    .hovered(rect.contains(self.last_cursor)),
            );
        }
        // The well is inset `S1` from each stepper so the three controls
        // keep a token gap; the hit rect (`l.service_sel`) stays contiguous.
        let well = Rect::new(
            l.service_sel.x0 + S1,
            l.service_sel.y0,
            l.service_sel.x1 - S1,
            l.service_sel.y1,
        );
        paint::card(scene, t, well, 7.0, theme.tokens.colors.surfaces.base);
        let w = paint::text_width(tr, name, 13.0);
        paint::label_v(
            &mut PaintCtx::new(scene, t, tr, theme),
            name,
            13.0,
            theme.text_primary,
            Rect {
                x0: well.x0 + (well.width() - w) / 2.0,
                x1: well.x0 + (well.width() - w) / 2.0 + w + 2.0,
                ..well
            },
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
            // The caption sits in the gap the layout left above the field
            // (`form_source_gap`), ending 2px above the field's top edge.
            paint::label(
                &mut PaintCtx::new(scene, t, tr, theme),
                "Source",
                11.0,
                theme.text_dim,
                Point::new(
                    l.source_field.x0,
                    l.source_field.y0 - self.metrics.form_source_gap + 10.0,
                ),
                120.0,
            );
        } else {
            paint::label_v(
                &mut PaintCtx::new(scene, t, tr, theme),
                "Source",
                12.0,
                theme.text_dim,
                Rect {
                    x0: card.x0 + 16.0,
                    x1: card.x0 + 16.0 + 60.0,
                    ..l.source_field
                },
            );
        }
        self.form.source.draw(scene, t, tr, theme, presence);
    }

    fn draw_form_add_button(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        l: &Layout,
    ) {
        Button::paint(
            scene,
            t,
            tr,
            theme,
            &ButtonFace::new(l.add_btn, "Add rule")
                .primary()
                .radius(8.0)
                .text_size(13.0)
                .hovered(l.add_btn.contains(self.last_cursor)),
        );
    }
}

/// Column x positions of the rules table. The header captions and every row
/// read the same struct, so a caption always starts over the text it
/// describes: `dot` is the status-marker centre and `text` is where the
/// target label begins after it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RuleColumns {
    dot: f64,
    text: f64,
    proto: f64,
    action: f64,
    source: f64,
    ctrl: f64,
}

/// Distance from the table's inner edge to the target text: room for the
/// 3px status dot at `+4` plus a gap. Deliberately off the spacing scale so
/// the dot reads as a bullet, not a column.
const RULE_TEXT_INSET: f64 = 14.0;

/// Resolve the rule-table column x positions from the table rect.
fn rule_columns(table: Rect) -> RuleColumns {
    let inner_x0 = table.x0 + 12.0;
    let inner_x1 = table.x1 - 12.0;
    let avail = inner_x1 - inner_x0;
    RuleColumns {
        dot: inner_x0 + 4.0,
        text: inner_x0 + RULE_TEXT_INSET,
        proto: inner_x0 + 0.30 * avail,
        action: inner_x0 + 0.42 * avail,
        source: inner_x0 + 0.56 * avail,
        ctrl: inner_x1 - 92.0,
    }
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

    use crate::resource::FirewallShell;
    use ui::prelude::TestApp;

    fn app_with_rect() -> FirewallApp {
        // Sample-populated build: these tests exercise rule/zone behaviour
        // that needs a ruleset, which only the demo build fabricates.
        let mut a = FirewallApp::new_demo();
        a.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        a.relayout();
        a
    }

    /// The app hosted on its resource, holding the sample ruleset: every
    /// control below reaches the document through the dispatcher.
    fn hosted() -> TestApp<FirewallShell> {
        let mut app = TestApp::<FirewallShell>::new().unwrap();
        app.invoke(
            ui::tilo_resource::Invoke::apply(&FirewallAction::Import {
                config: crate::sample::sample_config(),
            })
            .unwrap(),
        )
        .unwrap();
        app
    }

    /// The bounds of the control `target` names, as assistive technology
    /// sees it.
    fn bounds_of(node: &ui::headless::InspectionNode, target: &str) -> Option<Rect> {
        fn find(node: &AccessNode, target: &str) -> Option<Rect> {
            if node.action_target.as_deref() == Some(target) {
                return node.bounds.map(ui::AccessBounds::to_rect);
            }
            node.children.iter().find_map(|child| find(child, target))
        }
        node.semantics
            .as_ref()
            .and_then(|semantics| find(semantics, target))
            .or_else(|| {
                node.children
                    .iter()
                    .find_map(|child| bounds_of(child, target))
            })
    }

    /// Click the control `target` names, where a person sees it.
    fn operate(app: &mut TestApp<FirewallShell>, target: &str) {
        let root = app.harness().inspection().root;
        let bounds = bounds_of(&root, target).unwrap_or_else(|| panic!("{target} is shown"));
        let center = bounds.center();
        app.harness().click_at(center.x, center.y);
        app.settle();
    }

    /// The painted header controls are reachable through the action door.
    #[test]
    fn populated_firewall_passes_native_certification() {
        let driver = ui::headless::Driver::new(
            ui::State::new(),
            Box::new(FirewallApp::new_demo()),
            (1280, 800),
        );
        let audit = driver.native_app_audit();
        assert!(
            audit.is_certified(),
            "native={:?}, accessibility={:?}",
            audit.issues,
            audit.accessibility.issues
        );

        let mut app = hosted();
        let panic = app.state().panic_mode;
        operate(&mut app, "firewall.header.panic");
        assert_ne!(app.state().panic_mode, panic);
    }

    /// An unavailable firewall shows and announces its recovery, a failed
    /// recovery changes nothing, and the backend status is announced.
    #[test]
    fn backend_availability_governs_what_is_shown_and_announced() {
        // --- phase: nothing_shown_is_fabricated ---
        {
            // The honesty guard: before the resource presents a configuration
            // the surface shows none, never the fabricated sample.
            let live = FirewallApp::new();
            assert!(!live.available);
            assert!(live.config.zones.is_empty());
        }

        // --- phase: failed_recovery_changes_nothing_and_exports_retry_semantics ---
        {
            let mut app = FirewallApp::from_parts(FirewallConfig::empty(), false);
            app.rect = Rect::new(0.0, 0.0, 900.0, 600.0);
            app.relayout();

            let semantics = app.accessibility().expect("recovery semantics");
            assert!(semantics.children.iter().any(|child| {
                child.action_target.as_deref() == Some("firewall.retry")
                    && child.name.as_deref() == Some("Retry connection to firewalld")
            }));

            let failed = app.finish_retry(Err(
                "Still unable to reach firewalld. Start the service, authorize it, then retry."
                    .into(),
            ));
            assert!(failed.is_err());
            assert!(!app.available);
            assert!(app.config.zones.is_empty());
            assert!(app.status.contains("Still unable to reach firewalld"));
        }

        // --- phase: root_accessibility_announces_backend_status ---
        {
            let mut app = app_with_rect();
            app.status = "Rules applied".into();
            let node = app.accessibility().expect("firewall exports a window");
            assert_eq!(node.role, Role::Window);
            assert!(
                node.children
                    .iter()
                    .any(|child| child.name.as_deref() == Some("Rules applied"))
            );
        }
    }

    /// The zone list and rules table use shared semantics and are operable through accessibility.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one case of the same surface"
    )]
    #[test]
    fn zone_list_and_rules_table_are_reader_operable() {
        // --- phase: zone_list_uses_shared_semantics_and_is_reader_operable ---
        {
            let mut app = app_with_rect();
            let node = app.accessibility().expect("firewall semantics");
            let zones = node
                .children
                .iter()
                .find(|child| child.name.as_deref() == Some("Firewall zones"))
                .expect("zone list");
            assert_eq!(zones.role, Role::List);
            assert_eq!(zones.children.len(), ZoneKind::ALL.len());
            assert!(zones.children.iter().all(|zone| {
                zone.role == Role::ListItem
                    && zone.bounds.is_some()
                    && zone.pos_in_set.is_some()
                    && zone.set_size == Some(ZoneKind::ALL.len())
                    && zone.action_target.is_some()
            }));

            let mut doc = AutoCommit::new();
            assert!(app.perform_semantic_action(
                "firewall.zone:home",
                &A11yAction::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(app.selected_zone, ZoneKind::Home);
        }

        // --- phase: rules_table_uses_shared_semantics_and_rows_select_through_a11y ---
        {
            let mut app = app_with_rect();
            let node = app.accessibility().expect("firewall semantics");
            let table = node
                .children
                .iter()
                .find(|child| child.role == Role::Table)
                .expect("rules table");
            assert_eq!(table.children[0].name.as_deref(), Some("Column headers"));
            assert_eq!(table.children[0].children.len(), 5);
            let row = &table.children[1];
            assert_eq!(row.children[0].name.as_deref(), Some("Target"));
            assert!(row.children[0].value.is_some());
            let toggle = row
                .children
                .iter()
                .find(|child| child.role == Role::ToggleButton)
                .expect("rule toggle semantics");
            assert_eq!(toggle.state.checked, Some(true));
            assert!(toggle.action_target.is_some(), "rule toggle action target");
            let delete_target = row
                .children
                .iter()
                .find(|child| child.role == Role::Button)
                .and_then(|child| child.action_target.clone())
                .expect("delete action target");
            let target = row.action_target.clone().expect("selectable rule row");
            let id = target
                .strip_prefix("firewall.rule.select:")
                .and_then(|id| RuleId::parse_str(id).ok())
                .expect("rule id target");

            let mut doc = AutoCommit::new();
            assert!(app.perform_semantic_action(
                &target,
                &A11yAction::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(app.selected_rule, Some(id));
            let before = app.selected_zone_ref().map_or(0, |zone| zone.rules.len());
            assert!(app.perform_semantic_action(
                &delete_target,
                &A11yAction::Activate,
                &mut doc,
                &automerge::ROOT,
            ));
            assert_eq!(app.pending_delete, Some(id));
            assert_eq!(
                app.selected_zone_ref().map_or(0, |zone| zone.rules.len()),
                before
            );

            // Hosted, the same targets edit the stored configuration: the
            // toggle flips the rule, and a confirmed delete removes it.
            let mut app = hosted();
            let rule = app.state().zone(ZoneKind::Public).unwrap().rules[0].clone();
            operate(&mut app, &format!("firewall.rule.toggle:{}", rule.id));
            let toggled = app.state().zone(ZoneKind::Public).unwrap().rules[0].clone();
            assert_eq!(toggled.id, rule.id);
            assert_ne!(toggled.enabled, rule.enabled);
            let delete = format!("firewall.rule.delete:{}", rule.id);
            operate(&mut app, &delete);
            assert!(
                app.state()
                    .zone(ZoneKind::Public)
                    .unwrap()
                    .rules
                    .iter()
                    .any(|r| r.id == rule.id),
                "the first press only asks for confirmation"
            );
            operate(&mut app, &delete);
            assert!(
                !app.state()
                    .zone(ZoneKind::Public)
                    .unwrap()
                    .rules
                    .iter()
                    .any(|r| r.id == rule.id)
            );
        }
    }

    /// Relayout builds every zone row, header columns and service hit rects follow the built rows, and selected rows register on the raised card.
    #[test]
    fn table_geometry_follows_the_built_rows() {
        // --- phase: relayout_produces_all_nine_zone_rows ---
        {
            let a = app_with_rect();
            assert_eq!(a.layout.zone_rows.len(), 9);
        }

        // --- phase: table_header_columns_follow_the_built_rows ---
        // The header captions and the rows read one `RuleColumns`; this pins the
        // two places that can still drift: the `ON` caption over the toggles the
        // layout actually built, and the target caption starting at the text
        // column rather than under the status dot.
        {
            for width in [1200.0, 480.0] {
                let mut app = FirewallApp::new_demo();
                app.rect = Rect::new(0.0, 0.0, width, 800.0);
                app.relayout();
                let l = &app.layout;
                let cols = rule_columns(l.table);
                let first = l.rule_rows.first().expect("demo zone has rules");
                assert!(
                    (cols.ctrl - first.toggle.x0).abs() < 1e-6,
                    "ON caption must start over the row toggles at width {width}"
                );
                assert!(
                    cols.text > cols.dot,
                    "target caption sits after the status dot, over the row text"
                );
                assert!(
                    first.rect.x0 <= cols.dot && cols.text < cols.proto,
                    "columns stay inside the row at width {width}"
                );
            }
        }

        // --- phase: service_selector_hit_rects_are_contiguous ---
        // The service stepper's three controls hit-test as one contiguous strip
        // — no dead zone between `<`, the value and `>` — while the painted
        // value well is inset a token step from each stepper so the controls
        // never touch (HIG §7). Wide and stacked layouts alike.
        {
            for width in [1280.0, 800.0] {
                let (mut app, _) = measured_app(width, 1.0);
                app.form.is_service = true;
                app.relayout();
                let l = &app.layout;
                assert!(
                    (l.service_prev.x1 - l.service_sel.x0).abs() < 1e-6,
                    "no dead zone after `<` at {width}px"
                );
                assert!(
                    (l.service_sel.x1 - l.service_next.x0).abs() < 1e-6,
                    "no dead zone before `>` at {width}px"
                );
                assert!(
                    l.service_sel.width() > 2.0 * S1 + 60.0,
                    "the inset value well still has room for a service name at {width}px"
                );
            }
        }

        // --- phase: selected_rows_register_on_the_raised_card_on_every_preset ---
        // A selected rule row sits on the raised table card. `selection_tier`'s
        // lift ends at `raised` — the card's own colour on the light preset —
        // so the row takes a wash measured against the card instead, and that
        // wash registers on every preset (HIG §9.5's Δ-luminance bar) with the
        // ring as the stronger step.
        {
            use ui::surface::edge_delta;
            use ui::tokens::colors::HAIRLINE_MIN_DELTA;
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let raised = theme.tokens.colors.surfaces.raised;
                let (fill, edge) = selected_on(raised);
                assert!(
                    edge_delta(fill, raised) >= HAIRLINE_MIN_DELTA,
                    "the selected fill registers on the card"
                );
                assert!(
                    edge_delta(edge, raised) >= edge_delta(fill, raised),
                    "the ring is at least as strong a step as the fill"
                );
            }
            // The case the wash exists for: on the light preset the tier's lift
            // is the card itself, so it would not register at all.
            let light = Theme::light();
            let tier = selection_tier(&light, SelectionState::active(true));
            assert!(
                edge_delta(tier.fill, light.tokens.colors.surfaces.raised) < HAIRLINE_MIN_DELTA
            );
        }
    }

    /// Lay the demo app out at `width` × 800 with renderer-measured metrics at
    /// `scale`, exactly as the host does (`adjust_layout_with_measure`, then
    /// `apply_layout` → `relayout`).
    fn measured_app(width: f64, scale: f32) -> (FirewallApp, TextRenderer) {
        let mut tr = TextRenderer::new();
        tr.set_text_scale(scale);
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut app = FirewallApp::new_demo();
        app.rect = Rect::new(0.0, 0.0, width, 800.0);
        app.adjust_layout_with_measure(&mut taffy, node, &mut tr);
        app.relayout();
        (app, tr)
    }

    /// Summary cards, bands, breakpoints and the compact services route follow the text scale.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn layout_grows_with_the_text_scale() {
        // --- phase: summary_card_height_follows_the_wrapped_chip_rows ---
        // The summary card is as tall as its chip rows need: one row at desktop
        // width keeps the 134px floor, and the phone width — where the five chips
        // wrap onto two rows — grows by exactly one chip row instead of reserving
        // the old fixed 188px band (which left ~60px blank under the chips).
        {
            let (wide, _) = measured_app(1280.0, 1.0);
            let chips = &wide.layout.stat_chips;
            assert_eq!(chips.len(), 5, "public zone has five stat chips");
            assert!(
                chips.iter().all(|c| (c.y0 - chips[0].y0).abs() < 1e-6),
                "desktop width keeps every chip on one row"
            );
            assert!((wide.layout.summary.height() - SUMMARY_H).abs() < 1e-6);
            assert!(
                chips
                    .iter()
                    .all(|c| c.x1 <= wide.layout.summary.x1 - 16.0 + 1e-6),
                "chips stay inside the card"
            );

            // 480px: the five chips still fit one row, so the card keeps the
            // one-row height instead of the fixed 188px narrow band that
            // reserved a wrap which never happened.
            let (narrow, _) = measured_app(480.0, 1.0);
            let chips = &narrow.layout.stat_chips;
            assert!(chips.iter().all(|c| (c.y0 - chips[0].y0).abs() < 1e-6));
            assert!((narrow.layout.summary.height() - SUMMARY_H).abs() < 1e-6);

            // 360px: they wrap, and the card grows by exactly the rows it needs.
            let (phone, _) = measured_app(360.0, 1.0);
            let chips = &phone.layout.stat_chips;
            assert!(
                chips.iter().any(|c| (c.y0 - chips[0].y0).abs() > 1e-6),
                "five chips wrap onto a second row at 360px"
            );
            let last = chips.last().unwrap();
            assert!(
                (phone.layout.summary.y1 - last.y1 - PAD).abs() < 1e-6,
                "the card ends one padding below the last chip row, not ~60px later"
            );
            assert!(phone.layout.summary.height() < 188.0);
            assert!(
                chips
                    .iter()
                    .all(|c| c.x1 <= phone.layout.summary.x1 - 16.0 + 1e-6),
                "wrapped chips never run off the card's right edge"
            );
            // Everything below the card follows it.
            assert!(phone.layout.filter_seg[0].y0 >= phone.layout.summary.y1 + PAD - 1e-6);
        }

        // --- phase: bands_hold_their_1x_floors_and_grow_with_the_text_scale ---
        // At 1× the measured bands are exactly the constants the layout was
        // drawn for; at 2× they grow with the type and no control overlaps its
        // neighbour or leaves its card.
        {
            let (one, _) = measured_app(1280.0, 1.0);
            let m = &one.metrics;
            let floors = Metrics::default();
            assert!((m.ctrl_h - CTRL_H).abs() < 1e-6);
            assert!((m.header_h - HEADER_H).abs() < 1e-6);
            assert!((m.rule_row_h - RULE_ROW_H).abs() < 1e-6);
            assert!((m.rule_row_h_narrow - RULE_ROW_H_NARROW).abs() < 1e-6);
            assert!((m.table_head_h - floors.table_head_h).abs() < 1e-6);
            assert!((m.svc_row_h - SVC_ROW_H).abs() < 1e-6);
            assert!((m.seg_h - floors.seg_h).abs() < 1e-6);
            assert!((m.rail_row_h - floors.rail_row_h).abs() < 1e-6);
            assert!((m.chip_h - floors.chip_h).abs() < 1e-6);
            assert!((one.layout.form.height() - FORM_H).abs() < 1e-6);
            assert!((one.layout.header.height() - HEADER_H).abs() < 1e-6);
            // The first (unmeasured) frame lays out exactly like the measured 1×
            // frame: no jump on open.
            let mut unmeasured = FirewallApp::new_demo();
            unmeasured.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            unmeasured.relayout();
            assert_eq!(unmeasured.layout.form, one.layout.form);
            assert_eq!(unmeasured.layout.table, one.layout.table);
            assert_eq!(unmeasured.layout.header, one.layout.header);

            // 2400px at 2× is the wide layout (the breakpoint scales too).
            let (two, mut tr) = measured_app(2400.0, 2.0);
            let m = &two.metrics;
            assert!(m.ctrl_h > CTRL_H && m.header_h > HEADER_H && m.rule_row_h > RULE_ROW_H);
            let l = &two.layout;
            assert!(!l.narrow);
            assert!(
                l.form.height() > FORM_H,
                "form grows with its control bands"
            );
            // Header controls fit their labels and stay inside the header band.
            for b in [l.enable_pill, l.panic_btn, l.apply_btn] {
                assert!(b.y0 >= l.header.y0 && b.y1 <= l.header.y1);
            }
            assert!(
                l.enable_pill.width() > 92.0,
                "the enable pill widens for its 2× label"
            );
            // Form: line 1 clears the title band, line 2 clears line 1, the status
            // band clears line 2, and nothing leaves the card.
            let title_h = f64::from(tr.measure("Hg", 13.0, None).1);
            assert!(l.kind_seg[0].y0 >= l.form.y0 + 12.0 + title_h);
            assert!(l.source_field.y0 >= l.kind_seg[0].y1);
            assert!(l.add_btn.y1 + m.form_status_h <= l.form.y1 + 1e-6);
            #[expect(
                clippy::suspicious_operation_groupings,
                reason = "two independent containment checks on different controls"
            )]
            let action_and_kind_fit =
                l.action_seg[1].x1 <= l.form.x1 && l.kind_seg[1].x1 < l.port_field.x0;
            assert!(action_and_kind_fit);
            // Rows never overlap and the first clears the caption band.
            assert!(l.rule_rows[0].rect.y0 >= l.table.y0 + m.table_head_h);
            for pair in l.rule_rows.windows(2) {
                assert!(pair[1].rect.y0 >= pair[0].rect.y1);
            }

            // The stacked 2× form: four control lines, the `Source` caption's gap
            // and the title band all clear each other inside the card.
            let (stacked, _) = measured_app(800.0, 2.0);
            let l = &stacked.layout;
            assert!(l.narrow);
            assert!(l.kind_seg[0].y0 >= l.form.y0 + stacked.metrics.form_title_h_narrow - 1e-6);
            assert!(l.service_sel.y0.max(l.port_field.y0) >= l.kind_seg[0].y1);
            assert!(
                l.source_field.y0
                    >= l.port_field.y1.max(l.service_sel.y1) + stacked.metrics.form_source_gap
                        - 1e-6
            );
            assert!(l.add_btn.y0 >= l.source_field.y1 && l.add_btn.y1 <= l.form.y1 + 1e-6);
        }

        // --- phase: breakpoints_follow_the_text_scale ---
        // The narrow / services breakpoints follow the text scale: an 800px
        // window at 2× has the room of a 400px window at 1×, so it stacks the
        // form and takes two-line rows instead of crushing five columns.
        {
            let (one, _) = measured_app(800.0, 1.0);
            assert!(!one.layout.narrow);
            let (two, _) = measured_app(800.0, 2.0);
            assert!(two.layout.narrow, "800px at 2× takes the narrow layout");
            assert!(!two.layout.services_visible);
            let (wide, _) = measured_app(2400.0, 2.0);
            assert!(!wide.layout.narrow && wide.layout.services_visible);
        }

        // --- phase: compact_services_route_keeps_the_entire_catalog_interactive ---
        {
            for width in [640.0, 900.0] {
                let mut app = app_with_rect();
                app.rect = Rect::new(0.0, 0.0, width, 800.0);
                app.compact_view = CompactView::Services;
                app.relayout();

                assert!(app.layout.compact_services);
                assert_eq!(app.layout.content_seg.len(), 2);
                assert_eq!(app.layout.service_rows.len(), ServiceCatalog::ALL.len());
                let semantics = app.accessibility().expect("compact services semantics");
                let services = semantics
                    .children
                    .iter()
                    .find(|node| node.name.as_deref() == Some("Services in public"))
                    .expect("compact services list");
                assert_eq!(services.children.len(), ServiceCatalog::ALL.len());
                assert!(services.children.iter().all(|row| {
                    row.role == Role::ListItem
                        && row.pos_in_set.is_some()
                        && row.set_size == Some(ServiceCatalog::ALL.len())
                        && row.children.iter().any(|control| {
                            control.role == Role::Switch
                                && control.action_target.is_some()
                                && control.state.checked.is_some()
                        })
                }));

                let mut doc = AutoCommit::new();
                assert!(app.perform_semantic_action(
                    "firewall.service.toggle:public:ssh",
                    &A11yAction::ToggleCheck,
                    &mut doc,
                    &automerge::ROOT,
                ));
            }
        }
    }

    /// Filtering and zone switching stay presentation; service, zone and start/stop edits reach the stored configuration and undo.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    fn rule_and_zone_edits_round_trip_through_the_document() {
        // --- phase: filter_restricts_rule_rows ---
        {
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

        // --- phase: presentation_changes_do_not_create_document_versions ---
        {
            let mut app = hosted();
            let versions = app.versions().len();
            // Selection, filters, drawer geometry and draft focus are
            // deliberately per-window presentation state, never shared data.
            operate(&mut app, "firewall.zone:home");
            assert_eq!(app.versions().len(), versions);
        }

        // --- phase: cycle_zone_wraps ---
        {
            let mut a = app_with_rect();
            a.select_zone(ZoneKind::ALL[0]);
            a.cycle_zone(-1);
            assert_eq!(a.selected_zone, ZoneKind::ALL[8]);
            a.cycle_zone(1);
            assert_eq!(a.selected_zone, ZoneKind::ALL[0]);
        }

        // --- phase: an_invalid_draft_is_refused_before_it_reaches_the_document ---
        {
            let mut a = app_with_rect();
            a.form.is_service = false;
            a.form.port.set_text("70000");
            a.add_rule_from_form();
            assert!(a.status.starts_with("Rule not added"), "{}", a.status);
            assert!(a.selected_rule.is_none());
        }

        // --- phase: edits_reach_the_stored_configuration ---
        {
            let mut app = hosted();
            operate(&mut app, "firewall.service.toggle:public:ssh");
            assert!(
                !app.state()
                    .zone(ZoneKind::Public)
                    .unwrap()
                    .has_service("ssh"),
                "ssh is on in the sample and the toggle turned it off"
            );

            operate(&mut app, "firewall.zone:home");
            operate(&mut app, "firewall.header.enabled");
            assert!(!app.state().enabled);

            // Undo takes an edit back like any other.
            assert!(app.undo());
            assert!(app.state().enabled);
        }
    }

    /// Applying now runs through the same door as the palette and reports
    /// how it ended; with the live apply not armed it only plans.
    #[test]
    fn apply_now_reports_the_planned_commands() {
        let mut app = hosted();
        app.effect(&FirewallEffect::ApplyConfig).unwrap();
        app.settle();
        let frame = app.harness().inspection_json().unwrap();
        assert!(frame.contains("commands planned"), "{frame}");
    }
}
