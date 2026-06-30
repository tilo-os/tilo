//! The reusable permission editor — a single custom `impl Widget`.
//!
//! It is *reusable* in two senses: the standalone `permissions` binary
//! wraps it as its root, and any host (Settings, Chat's "Server settings
//! → Roles") embeds the same [`EditorView`] via
//! [`crate::seam::open_editor`]. The view is constructed from an
//! [`EditorIntent`] so the entry point is identical everywhere.
//!
//! Layout, like the sibling apps, is hand-computed: `apply_layout`
//! populates hit rects, `draw` renders with the [`TextRenderer`] +
//! [`Theme`], and `handle_event` dispatches clicks. State lives in the
//! [`PermissionSet`] mirrored to the CRDT via [`PermissionsDoc`]; the
//! registry is held in-memory (it is code-contributed, not user data).
//!
//! Two screens:
//!   * **Roles** — list of roles (reorderable) on the left; on the right,
//!     the selected role's allow/deny/inherit grid, grouped by scope and
//!     filtered by a search box, plus name/color/hierarchy controls.
//!   * **Members** — list of members; on the right, their role
//!     checkboxes, individual overrides, and an "effective permissions"
//!     preview that runs the real [`check`] resolver.
//!
//! A context selector in the header scopes the per-instance permissions
//! (Chat servers) so a user edits "Manage Channels in #engineering"
//! distinctly from the global grid.

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::ColorCompat;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};

use crate::check::{Source, explain};
use crate::doc::PermissionsDoc;
use crate::model::{
    ContextId, Decision, Grant, Member, MemberId, PermissionSet, Role, RoleId, Scope,
};
use crate::registry::{PermissionDef, PermissionRegistry};
use crate::sample;
use crate::seam::EditorIntent;

const TABBAR_H: f64 = 52.0;
const LIST_W: f64 = 240.0;
const PAD: f64 = 16.0;
const ROW_H: f64 = 40.0;
const GRID_ROW_H: f64 = 34.0;
const SEG_W: f64 = 56.0; // width of one allow/deny/inherit segment

/// Which screen the editor is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorTab {
    Roles,
    Members,
}

/// Named hit rects, recomputed every `apply_layout`.
#[derive(Default, Clone)]
struct Hit {
    tab_roles: Rect,
    tab_members: Rect,
    context_chip: Rect,
    new_item: Rect,
    // role detail
    role_up: Rect,
    role_down: Rect,
    role_delete: Rect,
    role_swatch: Rect,
    // list rows: (entity index in the *visible* list, rect)
    rows: Vec<Rect>,
    // permission grid: (permission id, segment) → rect
    grid_cells: Vec<(String, Grant, Rect)>,
    // member role checkboxes: (role id, rect)
    role_checks: Vec<(RoleId, Rect)>,
    // member override cells: (permission id, segment, rect)
    override_cells: Vec<(String, Grant, Rect)>,
    // context picker menu rows
    context_rows: Vec<(Option<ContextId>, Rect)>,
}

/// A floating selector (only one open at a time).
enum Popup {
    /// Pick the active dynamic context (or "Global"). Geometry lives in
    /// `Hit::context_rows`, computed when the picker opens.
    ContextPicker,
    /// Pick a color for the selected role; `anchor` positions the grid.
    ColorPicker { anchor: Rect },
}

/// The editor widget. Holds the in-memory mirror of the CRDT state plus
/// the registry and transient UI state (selection, scroll, search).
pub struct EditorView {
    registry: PermissionRegistry,
    set: PermissionSet,

    tab: EditorTab,
    selected_role: Option<RoleId>,
    selected_member: Option<MemberId>,
    /// Which dynamic context the per-instance grid is scoped to. `None` =
    /// the global grid (non-dynamic permissions).
    active_context: Option<ContextId>,

    search: TextInput,
    popup: Option<Popup>,

    // geometry
    rect: Rect,
    list_rect: Rect,
    detail_rect: Rect,
    list_scroll: f64,
    detail_scroll: f64,
    detail_content_h: f64,
    hit: Hit,

    last_mouse: Point,
    dirty: bool,
    relayout: bool,
    seeded: bool,
    /// Set when a structural edit needs to be flushed to the CRDT on the
    /// next `sync`/event so peers converge.
    pending_write: bool,
}

impl Default for EditorView {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorView {
    /// A fresh editor with the sample registry loaded. The role/member
    /// data is seeded into the CRDT on first `sync` if the document is
    /// empty (matching the contacts app's seed-once pattern).
    pub fn new() -> Self {
        EditorView {
            registry: sample::sample_registry(),
            set: PermissionSet::default(),
            tab: EditorTab::Roles,
            selected_role: None,
            selected_member: None,
            active_context: None,
            search: TextInput::local().with_id("permissions_search"),
            popup: None,
            rect: Rect::ZERO,
            list_rect: Rect::ZERO,
            detail_rect: Rect::ZERO,
            list_scroll: 0.0,
            detail_scroll: 0.0,
            detail_content_h: 0.0,
            hit: Hit::default(),
            last_mouse: Point::ZERO,
            dirty: true,
            relayout: true,
            seeded: false,
            pending_write: false,
        }
    }

    /// Construct from a [`EditorIntent`] — the shared entry point used by
    /// the standalone app and every in-context host.
    pub fn from_intent(intent: EditorIntent) -> Self {
        let mut v = Self::new();
        match intent {
            EditorIntent::Roles { select } => {
                v.tab = EditorTab::Roles;
                v.selected_role = select;
            }
            EditorIntent::Members { select } => {
                v.tab = EditorTab::Members;
                v.selected_member = select;
            }
            EditorIntent::Context { context, tab } => {
                v.tab = tab;
                v.active_context = Some(context);
            }
        }
        v
    }

    /// Override the registry (a host injects the live, app-contributed
    /// registry instead of the sample one).
    pub fn with_registry(mut self, registry: PermissionRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// Read-only access for tests/hosts.
    pub fn set(&self) -> &PermissionSet {
        &self.set
    }
    pub fn registry(&self) -> &PermissionRegistry {
        &self.registry
    }
    pub fn active_tab(&self) -> EditorTab {
        self.tab
    }

    // -----------------------------------------------------------------
    // Selection helpers
    // -----------------------------------------------------------------

    fn visible_roles(&self) -> Vec<RoleId> {
        // Highest position first (strongest at the top of the list).
        let mut rs: Vec<&Role> = self.set.roles.iter().collect();
        rs.sort_by(|a, b| b.position.cmp(&a.position));
        rs.into_iter().map(|r| r.id).collect()
    }

    fn visible_members(&self) -> Vec<MemberId> {
        self.set.members.iter().map(|m| m.id).collect()
    }

    fn ensure_selection(&mut self) {
        if self.selected_role.is_none() {
            self.selected_role = self.visible_roles().first().copied();
        }
        if self.selected_member.is_none() {
            self.selected_member = self.visible_members().first().copied();
        }
    }

    /// Permissions to show in the right-hand grid, honoring the search
    /// box and the active context. When a context is active, only the
    /// per-instance (`DynamicInstance`) permissions matching that
    /// context's app+kind are shown; otherwise the non-dynamic ones are.
    fn grid_permissions(&self) -> Vec<PermissionDef> {
        let q = self.search.value();
        let active_ctx_meta = self
            .active_context
            .as_ref()
            .and_then(|id| self.set.context(id))
            .map(|c| (c.app.clone(), c.kind.clone()));
        self.registry
            .search(q)
            .into_iter()
            .filter(|d| match (&d.scope, &active_ctx_meta) {
                // Context active → show only matching per-instance perms.
                (Scope::DynamicInstance { app, kind }, Some((a, k))) => app == a && kind == k,
                (Scope::DynamicInstance { .. }, None) => false,
                // No context → hide dynamic perms (they need a context).
                (_, Some(_)) => false,
                (_, None) => true,
            })
            .cloned()
            .collect()
    }

    /// Group a permission list by scope, preserving registration order of
    /// the scopes. Returns `(scope, perms)` pairs.
    fn group_by_scope(perms: &[PermissionDef]) -> Vec<(Scope, Vec<PermissionDef>)> {
        let mut groups: Vec<(Scope, Vec<PermissionDef>)> = Vec::new();
        for d in perms {
            if let Some(g) = groups.iter_mut().find(|(s, _)| s == &d.scope) {
                g.1.push(d.clone());
            } else {
                groups.push((d.scope.clone(), vec![d.clone()]));
            }
        }
        groups
    }

    // -----------------------------------------------------------------
    // Mutations (all flag `pending_write` so the next frame flushes CRDT)
    // -----------------------------------------------------------------

    fn create_role(&mut self) {
        let n = self.set.roles.len();
        let color = sample_color(n);
        let role = Role::new(format!("New Role {}", n + 1), color, self.set.next_position());
        let id = role.id;
        self.set.roles.push(role);
        self.selected_role = Some(id);
        self.tab = EditorTab::Roles;
        self.mark_changed();
    }

    fn create_member(&mut self) {
        let mem = Member::new(format!("New Member {}", self.set.members.len() + 1));
        let id = mem.id;
        self.set.members.push(mem);
        self.selected_member = Some(id);
        self.tab = EditorTab::Members;
        self.mark_changed();
    }

    fn delete_selected_role(&mut self) {
        if let Some(id) = self.selected_role {
            self.set.remove_role(id);
            self.selected_role = self.visible_roles().first().copied();
            self.mark_changed();
        }
    }

    fn toggle_member_role(&mut self, role: RoleId) {
        if let Some(id) = self.selected_member {
            if let Some(m) = self.set.member_mut(id) {
                m.toggle_role(role);
                self.mark_changed();
            }
        }
    }

    fn reorder_selected(&mut self, delta: i64) {
        if let Some(id) = self.selected_role {
            self.set.reorder(id, delta);
            self.mark_changed();
        }
    }

    fn mark_changed(&mut self) {
        self.pending_write = true;
        self.dirty = true;
        self.relayout = true;
    }

    fn flush(&mut self, doc: &mut AutoCommit) {
        if self.pending_write {
            PermissionsDoc::write(doc, &self.set);
            self.pending_write = false;
        }
    }
}

// =====================================================================
// Layout
// =====================================================================

impl EditorView {
    fn layout(&mut self) {
        let r = self.rect;
        self.hit = Hit::default();

        // Tab bar across the top.
        let tab_y = r.y0 + 10.0;
        self.hit.tab_roles = Rect::new(r.x0 + PAD, tab_y, r.x0 + PAD + 92.0, tab_y + 32.0);
        self.hit.tab_members =
            Rect::new(self.hit.tab_roles.x1 + 8.0, tab_y, self.hit.tab_roles.x1 + 8.0 + 108.0, tab_y + 32.0);

        // Context chip (right of the tabs) + New button (far right).
        self.hit.new_item = Rect::new(r.x1 - PAD - 120.0, tab_y, r.x1 - PAD, tab_y + 32.0);
        self.hit.context_chip = Rect::new(
            self.hit.new_item.x0 - 12.0 - 200.0,
            tab_y,
            self.hit.new_item.x0 - 12.0,
            tab_y + 32.0,
        );

        let content_top = r.y0 + TABBAR_H;
        self.list_rect = Rect::new(r.x0, content_top, r.x0 + LIST_W, r.y1);
        self.detail_rect = Rect::new(self.list_rect.x1 + 1.0, content_top, r.x1, r.y1);

        // List rows.
        let count = match self.tab {
            EditorTab::Roles => self.visible_roles().len(),
            EditorTab::Members => self.visible_members().len(),
        };
        let body_top = self.list_rect.y0 + PAD;
        let max_scroll = (count as f64 * ROW_H - (self.list_rect.height() - PAD)).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
        let mut y = body_top - self.list_scroll;
        for _ in 0..count {
            self.hit.rows.push(Rect::new(
                self.list_rect.x0 + 8.0,
                y,
                self.list_rect.x1 - 8.0,
                y + ROW_H - 6.0,
            ));
            y += ROW_H;
        }

        // Detail.
        match self.tab {
            EditorTab::Roles => self.layout_role_detail(),
            EditorTab::Members => self.layout_member_detail(),
        }

        // Search box lives in the detail header band.
        let sx = self.detail_rect.x0 + PAD;
        let sy = self.detail_rect.y0 + 56.0;
        let search_rect = Rect::new(sx, sy, self.detail_rect.x1 - PAD, sy + 34.0);
        self.search.apply_layout_external(search_rect);
    }

    fn layout_role_detail(&mut self) {
        let area = self.detail_rect;
        let x1 = area.x1 - PAD;
        // Hierarchy + delete controls in the header.
        let hy = area.y0 + 14.0;
        self.hit.role_delete = Rect::new(x1 - 80.0, hy, x1, hy + 30.0);
        self.hit.role_down = Rect::new(self.hit.role_delete.x0 - 8.0 - 30.0, hy, self.hit.role_delete.x0 - 8.0, hy + 30.0);
        self.hit.role_up = Rect::new(self.hit.role_down.x0 - 6.0 - 30.0, hy, self.hit.role_down.x0 - 6.0, hy + 30.0);
        // Color swatch sits at the far left of the header (drawn there too).
        self.hit.role_swatch = Rect::new(area.x0 + PAD, area.y0 + 16.0, area.x0 + PAD + 26.0, area.y0 + 42.0);

        // The grid begins below the search band.
        let perms = self.grid_permissions();
        let groups = Self::group_by_scope(&perms);
        let x0 = area.x0 + PAD;
        let seg_x = x1 - SEG_W * 3.0;
        let mut y = area.y0 + 104.0 - self.detail_scroll;
        for (_scope, list) in &groups {
            y += 30.0; // group header band
            for d in list {
                let cell = |i: usize| Rect::new(seg_x + i as f64 * SEG_W, y, seg_x + (i as f64 + 1.0) * SEG_W, y + GRID_ROW_H - 6.0);
                self.hit.grid_cells.push((d.id.clone(), Grant::Allow, cell(0)));
                self.hit.grid_cells.push((d.id.clone(), Grant::Inherit, cell(1)));
                self.hit.grid_cells.push((d.id.clone(), Grant::Deny, cell(2)));
                y += GRID_ROW_H;
            }
            y += 8.0;
        }
        let _ = x0;
        self.detail_content_h = (y + self.detail_scroll) - (area.y0 + 104.0) + 40.0;
    }

    fn layout_member_detail(&mut self) {
        let area = self.detail_rect;
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let mut y = area.y0 + 104.0 - self.detail_scroll;

        // Role checkboxes — one chip per role.
        y += 26.0; // "ROLES" header
        let roles = self.visible_roles();
        let mut cx = x0;
        let chip_w = 150.0;
        for rid in &roles {
            if cx + chip_w > x1 {
                cx = x0;
                y += 36.0;
            }
            self.hit.role_checks.push((*rid, Rect::new(cx, y, cx + chip_w, y + 30.0)));
            cx += chip_w + 8.0;
        }
        y += 30.0 + 18.0;

        // Override grid (same shape as the role grid).
        y += 26.0; // "OVERRIDES" header
        let perms = self.grid_permissions();
        let groups = Self::group_by_scope(&perms);
        let seg_x = x1 - SEG_W * 3.0;
        for (_scope, list) in &groups {
            y += 28.0;
            for d in list {
                let cell = |i: usize| Rect::new(seg_x + i as f64 * SEG_W, y, seg_x + (i as f64 + 1.0) * SEG_W, y + GRID_ROW_H - 6.0);
                self.hit.override_cells.push((d.id.clone(), Grant::Allow, cell(0)));
                self.hit.override_cells.push((d.id.clone(), Grant::Inherit, cell(1)));
                self.hit.override_cells.push((d.id.clone(), Grant::Deny, cell(2)));
                y += GRID_ROW_H;
            }
            y += 8.0;
        }
        self.detail_content_h = (y + self.detail_scroll) - (area.y0 + 104.0) + 40.0;
    }
}

// =====================================================================
// Drawing
// =====================================================================

impl EditorView {
    fn render(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);
        self.draw_tabbar(scene, t, tr, theme);
        self.draw_list(scene, t, tr, theme);
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.detail_rect);
        match self.tab {
            EditorTab::Roles => self.draw_role_detail(scene, t, tr, theme, presence),
            EditorTab::Members => self.draw_member_detail(scene, t, tr, theme),
        }
        if let Some(popup) = &self.popup {
            self.draw_popup(scene, t, tr, theme, popup);
        }
    }

    fn draw_tabbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TABBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((bar.x0, bar.y1), (bar.x1, bar.y1)));

        seg_button(scene, t, tr, theme, self.hit.tab_roles, "Roles", self.tab == EditorTab::Roles);
        seg_button(scene, t, tr, theme, self.hit.tab_members, "Members", self.tab == EditorTab::Members);

        // Context chip.
        let label = match &self.active_context {
            Some(id) => self
                .set
                .context(id)
                .map(|c| format!("\u{f0c1} {}", c.label))
                .unwrap_or_else(|| id.0.clone()),
            None => "\u{f0ac} Global".to_string(),
        };
        chip(scene, t, tr, theme, self.hit.context_chip, &label, self.active_context.is_some());

        let new_label = match self.tab {
            EditorTab::Roles => "+ New Role",
            EditorTab::Members => "+ New Member",
        };
        toolbar_button(scene, t, tr, theme, self.hit.new_item, new_label, true);
    }

    fn draw_list(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.list_rect);
        scene.stroke(
            &Stroke::new(1.0),
            t,
            c.border_standard,
            None,
            &Line::new((self.list_rect.x1, self.list_rect.y0), (self.list_rect.x1, self.list_rect.y1)),
        );
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &self.list_rect);
        match self.tab {
            EditorTab::Roles => {
                for (rid, rr) in self.visible_roles().into_iter().zip(self.hit.rows.iter()) {
                    let Some(role) = self.set.role(rid) else { continue };
                    let selected = self.selected_role == Some(rid);
                    self.draw_list_row(scene, t, tr, theme, *rr, &role.name, Some(role.color), selected, &format!("{} grants", role.grants.len()));
                }
            }
            EditorTab::Members => {
                for (mid, rr) in self.visible_members().into_iter().zip(self.hit.rows.iter()) {
                    let Some(m) = self.set.member(mid) else { continue };
                    let selected = self.selected_member == Some(mid);
                    let sub = format!("{} role{}", m.role_ids.len(), if m.role_ids.len() == 1 { "" } else { "s" });
                    self.draw_list_row(scene, t, tr, theme, *rr, &m.display_name, None, selected, &sub);
                }
            }
        }
        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_list_row(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rr: Rect, name: &str, color: Option<u32>, selected: bool, sub: &str) {
        let c = &theme.tokens.colors;
        if rr.y1 < self.list_rect.y0 || rr.y0 > self.list_rect.y1 {
            return;
        }
        if selected {
            let bg = RoundedRect::from_rect(rr, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &bg);
        }
        let tx = if let Some(rgb) = color {
            // Role color dot.
            let dot = vello::kurbo::Circle::new(Point::new(rr.x0 + 14.0, rr.center().y), 6.0);
            scene.fill(Fill::NonZero, t, packed_color(rgb), None, &dot);
            rr.x0 + 28.0
        } else {
            rr.x0 + 12.0
        };
        tr.draw_text_weighted(scene, name, 14.0, c.text_primary, t * Affine::translate((tx, rr.y0 + 4.0)), Some((rr.x1 - tx - 8.0) as f32), TextWeight::Medium);
        tr.draw_text(scene, sub, 11.0, c.text_dim, t * Affine::translate((tx, rr.y0 + 20.0)), None);
    }

    fn draw_role_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let Some(rid) = self.selected_role else {
            tr.draw_text(scene, "Select or create a role", 14.0, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 40.0)), None);
            return;
        };
        let Some(role) = self.set.role(rid).cloned() else { return };

        // Header: role name + color swatch + hierarchy/delete controls.
        let swatch = Rect::new(area.x0 + PAD, area.y0 + 16.0, area.x0 + PAD + 26.0, area.y0 + 42.0);
        let sw = RoundedRect::from_rect(swatch, 6.0);
        scene.fill(Fill::NonZero, t, packed_color(role.color), None, &sw);
        tr.draw_text_weighted(scene, &role.name, 20.0, c.text_primary, t * Affine::translate((swatch.x1 + 12.0, area.y0 + 18.0)), None, TextWeight::Bold);
        tr.draw_text(scene, &format!("position {}", role.position), 11.0, c.text_dim, t * Affine::translate((swatch.x1 + 12.0, area.y0 + 42.0)), None);

        toolbar_button(scene, t, tr, theme, self.hit.role_up, "\u{2191}", false);
        toolbar_button(scene, t, tr, theme, self.hit.role_down, "\u{2193}", false);
        danger_button(scene, t, tr, theme, self.hit.role_delete, "Delete");

        // Search box + placeholder.
        self.search.draw(scene, t, tr, theme, presence);
        if self.search.value().is_empty() && !self.search.is_focused() {
            tr.draw_text(scene, "Search permissions\u{2026}", 13.0, c.text_dim, t * Affine::translate((self.detail_rect.x0 + PAD + 12.0, self.detail_rect.y0 + 56.0 + 8.0)), None);
        }

        // Grid.
        let perms = self.grid_permissions();
        let groups = Self::group_by_scope(&perms);
        self.draw_grid(scene, t, tr, theme, &groups, |perm| role.grant_for(perm, self.active_context.as_ref()), area);
    }

    fn draw_member_detail(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let area = self.detail_rect;
        let Some(mid) = self.selected_member else {
            tr.draw_text(scene, "Select or create a member", 14.0, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 40.0)), None);
            return;
        };
        let Some(member) = self.set.member(mid).cloned() else { return };

        tr.draw_text_weighted(scene, &member.display_name, 20.0, c.text_primary, t * Affine::translate((area.x0 + PAD, area.y0 + 18.0)), None, TextWeight::Bold);
        let role_names: Vec<String> = self.set.roles_of(&member).iter().map(|r| r.name.clone()).collect();
        let sub = if role_names.is_empty() { "no roles".to_string() } else { role_names.join(", ") };
        tr.draw_text(scene, &sub, 12.0, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 44.0)), Some((area.width() - PAD * 2.0) as f32));

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &Rect::new(area.x0, area.y0 + 96.0, area.x1, area.y1));

        // ROLES section.
        tr.draw_text_weighted(scene, "ROLES", 11.0, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 104.0 - self.detail_scroll + 6.0)), None, TextWeight::Semibold);
        for (rid, rect) in &self.hit.role_checks {
            let Some(role) = self.set.role(*rid) else { continue };
            let held = member.has_role(*rid);
            role_chip(scene, t, tr, theme, *rect, &role.name, packed_color(role.color), held);
        }

        // OVERRIDES + effective preview share the grid; we draw a header
        // then the override grid showing the member's explicit overrides,
        // with the resolved effective decision badge per row.
        let perms = self.grid_permissions();
        let groups = Self::group_by_scope(&perms);
        let registry = &self.registry;
        let set = &self.set;
        let ctx = self.active_context.clone();
        // Override value accessor.
        let member_clone = member.clone();
        self.draw_override_grid(scene, t, tr, theme, &groups, area, &member_clone, registry, set, ctx.as_ref());

        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_grid(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        groups: &[(Scope, Vec<PermissionDef>)],
        value_of: impl Fn(&str) -> Grant,
        area: Rect,
    ) {
        let c = &theme.tokens.colors;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &Rect::new(area.x0, area.y0 + 100.0, area.x1, area.y1));
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let seg_x = x1 - SEG_W * 3.0;
        let mut y = area.y0 + 104.0 - self.detail_scroll;
        if groups.is_empty() {
            tr.draw_text(scene, "No permissions match.", 13.0, c.text_dim, t * Affine::translate((x0, y + 10.0)), None);
        }
        for (scope, list) in groups {
            // Group header.
            tr.draw_text_weighted(scene, &scope.label().to_uppercase(), 11.0, theme.accent_primary, t * Affine::translate((x0, y + 10.0)), None, TextWeight::Semibold);
            // Column captions on the first group.
            seg_caption(scene, t, tr, theme, seg_x, y + 8.0);
            y += 30.0;
            for d in list {
                let g = value_of(&d.id);
                tr.draw_text(scene, &d.label, 13.0, c.text_primary, t * Affine::translate((x0, y + 4.0)), Some((seg_x - x0 - 10.0) as f32));
                tr.draw_text(scene, &d.description, 10.0, c.text_dim, t * Affine::translate((x0, y + 19.0)), Some((seg_x - x0 - 10.0) as f32));
                tri_state(scene, t, tr, theme, seg_x, y, g);
                y += GRID_ROW_H;
            }
            y += 8.0;
        }
        scene.pop_layer();
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_override_grid(
        &self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        groups: &[(Scope, Vec<PermissionDef>)],
        area: Rect,
        member: &Member,
        registry: &PermissionRegistry,
        set: &PermissionSet,
        ctx: Option<&ContextId>,
    ) {
        let c = &theme.tokens.colors;
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let seg_x = x1 - SEG_W * 3.0;
        // Each permission's Allow-segment rect (the row anchor) lives in
        // the laid-out `override_cells`; drive drawing off that single
        // source of truth so the text and the clickable control can never
        // drift apart. The header band sits above the first row.
        let cell_y = |perm: &str| -> Option<f64> {
            self.hit
                .override_cells
                .iter()
                .find(|(p, g, _)| p == perm && *g == Grant::Allow)
                .map(|(_, _, r)| r.y0)
        };
        if let Some(first) = self.hit.override_cells.first().map(|(_, _, r)| r.y0) {
            let hy = first - 28.0;
            tr.draw_text_weighted(scene, "OVERRIDES  +  EFFECTIVE", 11.0, c.text_dim, t * Affine::translate((x0, hy)), None, TextWeight::Semibold);
            seg_caption(scene, t, tr, theme, seg_x, hy);
        }
        for (scope, list) in groups {
            // Group header above the group's first permission row.
            if let Some(first_perm) = list.first() {
                if let Some(y) = cell_y(&first_perm.id) {
                    tr.draw_text_weighted(scene, &scope.label().to_uppercase(), 10.0, theme.accent_primary, t * Affine::translate((x0, y - 16.0)), None, TextWeight::Semibold);
                }
            }
            for d in list {
                let Some(y) = cell_y(&d.id) else { continue };
                let ov = member.override_for(&d.id, ctx);
                // Effective decision + source via the real resolver.
                let res = explain(registry, set, member, &d.id, ctx);
                tr.draw_text(scene, &d.label, 13.0, c.text_primary, t * Affine::translate((x0, y + 4.0)), Some((seg_x - x0 - 90.0) as f32));
                effective_badge(scene, t, tr, theme, Point::new(seg_x - 84.0, y + 6.0), res.decision, &res.source);
                tri_state(scene, t, tr, theme, seg_x, y, ov);
            }
        }
    }

    fn draw_popup(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, popup: &Popup) {
        match popup {
            Popup::ContextPicker => {
                let rows = &self.hit.context_rows;
                if rows.is_empty() {
                    return;
                }
                let first = rows[0].1;
                let last = rows.last().unwrap().1;
                let panel = Rect::new(first.x0 - 6.0, first.y0 - 6.0, first.x1 + 6.0, last.y1 + 6.0);
                menu_panel(scene, t, theme, panel);
                for (ctx, rect) in rows {
                    let label = match ctx {
                        None => "Global".to_string(),
                        Some(id) => self.set.context(id).map(|c| c.label.clone()).unwrap_or_else(|| id.0.clone()),
                    };
                    let hov = rect.contains(self.last_mouse);
                    let sel = &self.active_context == ctx;
                    menu_item(scene, t, tr, theme, *rect, &label, hov, sel);
                }
            }
            Popup::ColorPicker { anchor } => {
                let (panel, swatches) = color_grid_geometry(*anchor);
                menu_panel(scene, t, theme, panel);
                for (i, sr) in swatches.iter().enumerate() {
                    let sh = RoundedRect::from_rect(*sr, 6.0);
                    scene.fill(Fill::NonZero, t, packed_color(ROLE_PALETTE[i]), None, &sh);
                    if sr.contains(self.last_mouse) {
                        scene.stroke(&Stroke::new(2.0), t, Color::WHITE, None, &sh);
                    }
                }
            }
        }
    }
}

// =====================================================================
// Widget impl
// =====================================================================

impl Widget for EditorView {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            let existing = PermissionsDoc::read(state);
            if existing.roles.is_empty() && existing.members.is_empty() {
                self.set = sample::sample_set();
                PermissionsDoc::write(state, &self.set);
            } else {
                self.set = existing;
            }
            self.ensure_selection();
            self.seeded = true;
            self.relayout = true;
            self.dirty = true;
        }
        self.flush(state);
        self.search.sync(state, path);
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) },
                ..Default::default()
            })
            .unwrap();
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
        self.layout();
        self.relayout = false;
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        self.render(scene, transform, tr, theme, presence);
    }

    fn draw_with_presence(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, _permissions: &PermissionContext) {
        self.render(scene, transform, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        if self.popup.is_some() {
            self.handle_popup(event, handled);
            if *handled {
                self.flush(state);
                return;
            }
        }

        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                self.handle_click(state, path, handled, node_id, taffy, modifiers);
                self.flush(state);
                return;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-y * 48.0) as f64,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                if self.list_rect.contains(self.last_mouse) {
                    self.list_scroll += dy;
                } else if self.detail_rect.contains(self.last_mouse) {
                    let max = (self.detail_content_h - self.detail_rect.height() + 120.0).max(0.0);
                    self.detail_scroll = (self.detail_scroll + dy).clamp(0.0, max);
                }
                self.relayout = true;
                self.dirty = true;
                *handled = true;
                return;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.handle_key(ke, state, path, handled, node_id, taffy, modifiers, event, scale_factor);
                self.flush(state);
                return;
            }
            _ => {}
        }

        // Forward pointer to the search input for hover/caret.
        let mut sink = None;
        self.search.handle_event(event, scale_factor, modifiers, state, path, handled, &mut sink, node_id, taffy);
    }

    fn name(&self) -> &str {
        "PermissionsEditor"
    }
    fn test_id(&self) -> Option<&str> {
        Some("PermissionsEditor")
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
    fn needs_redraw(&self) -> bool {
        self.dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
    }
    fn is_layout_dirty(&self) -> bool {
        self.relayout
    }
    fn needs_commit(&self) -> bool {
        self.search.needs_commit()
    }
    fn on_commit(&mut self) {
        self.search.on_commit();
    }
}

// =====================================================================
// Event handling
// =====================================================================

impl EditorView {
    #[allow(clippy::too_many_arguments)]
    fn handle_click(&mut self, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, node_id: NodeId, taffy: &TaffyTree<()>, modifiers: &winit::event::Modifiers) {
        let p = self.last_mouse;

        // Tabs.
        if self.hit.tab_roles.contains(p) {
            self.tab = EditorTab::Roles;
            self.detail_scroll = 0.0;
            self.mark_changed_view();
            *handled = true;
            return;
        }
        if self.hit.tab_members.contains(p) {
            self.tab = EditorTab::Members;
            self.detail_scroll = 0.0;
            self.mark_changed_view();
            *handled = true;
            return;
        }
        // Context chip → open the context picker.
        if self.hit.context_chip.contains(p) {
            self.open_context_picker();
            *handled = true;
            return;
        }
        // New.
        if self.hit.new_item.contains(p) {
            match self.tab {
                EditorTab::Roles => self.create_role(),
                EditorTab::Members => self.create_member(),
            }
            *handled = true;
            return;
        }

        // List rows.
        match self.tab {
            EditorTab::Roles => {
                for (rid, rr) in self.visible_roles().into_iter().zip(self.hit.rows.clone().into_iter()) {
                    if rr.contains(p) {
                        self.selected_role = Some(rid);
                        self.detail_scroll = 0.0;
                        self.mark_changed_view();
                        *handled = true;
                        return;
                    }
                }
            }
            EditorTab::Members => {
                for (mid, rr) in self.visible_members().into_iter().zip(self.hit.rows.clone().into_iter()) {
                    if rr.contains(p) {
                        self.selected_member = Some(mid);
                        self.detail_scroll = 0.0;
                        self.mark_changed_view();
                        *handled = true;
                        return;
                    }
                }
            }
        }

        // Role-detail controls.
        if self.tab == EditorTab::Roles {
            if self.hit.role_swatch.contains(p) && self.selected_role.is_some() {
                self.popup = Some(Popup::ColorPicker { anchor: self.hit.role_swatch });
                *handled = true;
                self.dirty = true;
                return;
            }
            if self.hit.role_up.contains(p) {
                self.reorder_selected(1);
                *handled = true;
                return;
            }
            if self.hit.role_down.contains(p) {
                self.reorder_selected(-1);
                *handled = true;
                return;
            }
            if self.hit.role_delete.contains(p) {
                self.delete_selected_role();
                *handled = true;
                return;
            }
            // Grid cells.
            if let Some((perm, seg, _)) = self.hit.grid_cells.iter().find(|(_, _, r)| r.contains(p)).cloned() {
                // Clicking a segment sets that exact value (not a blind cycle):
                // clicking Allow sets Allow, etc., unless it's already that value,
                // in which case it clears to Inherit.
                let current = self
                    .selected_role
                    .and_then(|id| self.set.role(id))
                    .map(|r| r.grant_for(&perm, self.active_context.as_ref()))
                    .unwrap_or(Grant::Inherit);
                let target = if current == seg { Grant::Inherit } else { seg };
                self.set_grant_direct(&perm, target);
                *handled = true;
                return;
            }
        }

        // Member-detail controls.
        if self.tab == EditorTab::Members {
            if let Some((rid, _)) = self.hit.role_checks.iter().find(|(_, r)| r.contains(p)).cloned() {
                self.toggle_member_role(rid);
                *handled = true;
                return;
            }
            if let Some((perm, seg, _)) = self.hit.override_cells.iter().find(|(_, _, r)| r.contains(p)).cloned() {
                let current = self
                    .selected_member
                    .and_then(|id| self.set.member(id))
                    .map(|m| m.override_for(&perm, self.active_context.as_ref()))
                    .unwrap_or(Grant::Inherit);
                let target = if current == seg { Grant::Inherit } else { seg };
                self.set_override_direct(&perm, target);
                *handled = true;
                return;
            }
        }

        // Otherwise forward to the search input (caret placement).
        let press = WindowEvent::MouseInput {
            device_id: unsafe { std::mem::zeroed() },
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let mut sink = None;
        let mut h = false;
        self.search.set_focus(self.search.get_rect().contains(p));
        if self.search.is_focused() {
            self.search.handle_event(&press, 1.0, modifiers, state, path, &mut h, &mut sink, node_id, taffy);
        }
        self.dirty = true;
        *handled = true;
    }

    /// Selection / tab / context changes need a redraw + relayout but are
    /// view-only (not persisted), so they don't dirty the CRDT.
    fn mark_changed_view(&mut self) {
        self.dirty = true;
        self.relayout = true;
    }

    fn set_grant_direct(&mut self, perm: &str, grant: Grant) {
        let ctx = self.active_context.clone();
        if let Some(id) = self.selected_role {
            if let Some(role) = self.set.role_mut(id) {
                role.set_grant(perm, ctx, grant);
                self.mark_changed();
            }
        }
    }

    fn set_override_direct(&mut self, perm: &str, grant: Grant) {
        let ctx = self.active_context.clone();
        if let Some(id) = self.selected_member {
            if let Some(m) = self.set.member_mut(id) {
                m.set_override(perm, ctx, grant);
                self.mark_changed();
            }
        }
    }

    fn open_context_picker(&mut self) {
        // Build context-picker rows: Global + every context.
        let anchor = self.hit.context_chip;
        let item_h = 32.0;
        let mut rows = vec![(None, Rect::new(anchor.x0, anchor.y1 + 4.0, anchor.x1, anchor.y1 + 4.0 + item_h))];
        let mut y = anchor.y1 + 4.0 + item_h;
        for ctx in &self.set.contexts {
            rows.push((Some(ctx.id.clone()), Rect::new(anchor.x0, y, anchor.x1, y + item_h)));
            y += item_h;
        }
        self.hit.context_rows = rows;
        self.popup = Some(Popup::ContextPicker);
        self.dirty = true;
    }

    fn handle_popup(&mut self, event: &WindowEvent, handled: &mut bool) {
        if matches!(event, WindowEvent::CursorMoved { .. }) {
            self.dirty = true;
            return;
        }
        let press = matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. });
        if !press {
            return;
        }
        let p = self.last_mouse;
        let popup = self.popup.take().unwrap();
        *handled = true;
        self.dirty = true;
        self.relayout = true;
        match popup {
            Popup::ContextPicker => {
                if let Some((ctx, _)) = self.hit.context_rows.iter().find(|(_, r)| r.contains(p)).cloned() {
                    self.active_context = ctx;
                    self.detail_scroll = 0.0;
                }
                self.hit.context_rows.clear();
            }
            Popup::ColorPicker { anchor } => {
                let (_panel, swatches) = color_grid_geometry(anchor);
                if let Some(i) = swatches.iter().position(|r| r.contains(p)) {
                    if let Some(id) = self.selected_role {
                        if let Some(role) = self.set.role_mut(id) {
                            role.color = ROLE_PALETTE[i];
                            self.mark_changed();
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn handle_key(
        &mut self,
        ke: &KeyEvent,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
        modifiers: &winit::event::Modifiers,
        full_event: &WindowEvent,
        scale_factor: f64,
    ) {
        if ke.state == ElementState::Pressed && matches!(ke.logical_key, Key::Named(NamedKey::Escape)) {
            if self.popup.is_some() {
                self.popup = None;
            } else {
                self.search.set_focus(false);
            }
            *handled = true;
            self.dirty = true;
            return;
        }
        // Route typing to the search box when focused (drives grid filter).
        if self.search.is_focused() {
            let mut sink = None;
            self.search.handle_event(full_event, scale_factor, modifiers, state, path, handled, &mut sink, node_id, taffy);
            self.relayout = true;
            self.dirty = true;
        }
    }
}

// =====================================================================
// Drawing primitives
// =====================================================================

fn packed_color(rgb: u32) -> Color {
    Color::from_rgb8(((rgb >> 16) & 0xff) as u8, ((rgb >> 8) & 0xff) as u8, (rgb & 0xff) as u8)
}

fn seg_button(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, active: bool) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if active {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    }
    let col = if active { Color::WHITE } else { c.text_primary };
    let tw = tr.measure(label, 13.0, None).0 as f64;
    tr.draw_text_weighted(scene, label, 13.0, col, t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - 8.0)), None, TextWeight::Medium);
}

fn toolbar_button(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, primary: bool) {
    if rect.width() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    if primary {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
    } else {
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    }
    let col = if primary { Color::WHITE } else { c.text_primary };
    let tw = tr.measure(label, 13.0, None).0 as f64;
    tr.draw_text_weighted(scene, label, 13.0, col, t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - 8.0)), None, TextWeight::Medium);
}

fn danger_button(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    scene.fill(Fill::NonZero, t, c.semantic.danger.with_alpha_factor(0.16), None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.semantic.danger, None, &shape);
    let tw = tr.measure(label, 13.0, None).0 as f64;
    tr.draw_text_weighted(scene, label, 13.0, c.semantic.danger, t * Affine::translate((rect.center().x - tw / 2.0, rect.center().y - 8.0)), None, TextWeight::Medium);
}

fn chip(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, active: bool) {
    if rect.width() <= 0.0 {
        return;
    }
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    let bg = if active { theme.accent_primary.with_alpha_factor(0.16) } else { c.surfaces.base };
    scene.fill(Fill::NonZero, t, bg, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    let col = if active { theme.accent_primary } else { c.text_primary };
    scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
    tr.draw_text(scene, label, 12.0, col, t * Affine::translate((rect.x0 + 10.0, rect.center().y - 7.0)), Some((rect.width() - 16.0) as f32));
    scene.pop_layer();
}

fn role_chip(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, color: Color, held: bool) {
    let c = &theme.tokens.colors;
    let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
    let bg = if held { color.with_alpha_factor(0.22) } else { c.surfaces.raised };
    scene.fill(Fill::NonZero, t, bg, None, &shape);
    let border = if held { color } else { c.border_standard };
    scene.stroke(&Stroke::new(if held { 1.6 } else { 1.0 }), t, border, None, &shape);
    // Check box.
    let box_r = Rect::new(rect.x0 + 8.0, rect.center().y - 7.0, rect.x0 + 22.0, rect.center().y + 7.0);
    let bsh = RoundedRect::from_rect(box_r, 3.0);
    if held {
        scene.fill(Fill::NonZero, t, color, None, &bsh);
        // tick
        let cx = box_r.center();
        let mut p = vello::kurbo::BezPath::new();
        p.move_to((cx.x - 4.0, cx.y));
        p.line_to((cx.x - 1.0, cx.y + 3.0));
        p.line_to((cx.x + 4.0, cx.y - 3.0));
        scene.stroke(&Stroke::new(1.8), t, Color::WHITE, None, &p);
    } else {
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &bsh);
    }
    tr.draw_text(scene, label, 13.0, c.text_primary, t * Affine::translate((box_r.x1 + 8.0, rect.center().y - 8.0)), Some((rect.x1 - box_r.x1 - 14.0) as f32));
}

fn seg_caption(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, seg_x: f64, y: f64) {
    let c = &theme.tokens.colors;
    for (i, cap) in ["Allow", "Inherit", "Deny"].iter().enumerate() {
        let cx = seg_x + i as f64 * SEG_W + SEG_W / 2.0;
        let tw = tr.measure(cap, 9.0, None).0 as f64;
        tr.draw_text(scene, cap, 9.0, c.text_dim, t * Affine::translate((cx - tw / 2.0, y)), None);
    }
}

/// Draw a three-segment Allow | Inherit | Deny control, highlighting the
/// active segment.
fn tri_state(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, seg_x: f64, y: f64, value: Grant) {
    let c = &theme.tokens.colors;
    let h = GRID_ROW_H - 8.0;
    let track = Rect::new(seg_x, y, seg_x + SEG_W * 3.0, y + h);
    let shape = RoundedRect::from_rect(track, theme.tokens.radii.sm as f64);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
    let (idx, fill) = match value {
        Grant::Allow => (0, c.semantic.success),
        Grant::Inherit => (1, c.text_dim),
        Grant::Deny => (2, c.semantic.danger),
    };
    let seg = Rect::new(seg_x + idx as f64 * SEG_W, y, seg_x + (idx as f64 + 1.0) * SEG_W, y + h);
    let segsh = RoundedRect::from_rect(seg.inset(-1.5), theme.tokens.radii.sm as f64);
    scene.fill(Fill::NonZero, t, fill.with_alpha_factor(if matches!(value, Grant::Inherit) { 0.18 } else { 0.9 }), None, &segsh);
    // Glyphs in each segment.
    for (i, glyph) in ["\u{2713}", "\u{2014}", "\u{2715}"].iter().enumerate() {
        let cx = seg_x + i as f64 * SEG_W + SEG_W / 2.0;
        let on = i == idx;
        let col = if on && !matches!(value, Grant::Inherit) {
            Color::WHITE
        } else if on {
            c.text_primary
        } else {
            c.text_dim.with_alpha_factor(0.5)
        };
        let tw = tr.measure(glyph, 13.0, None).0 as f64;
        tr.draw_text(scene, glyph, 13.0, col, t * Affine::translate((cx - tw / 2.0, track.center().y - 9.0)), None);
    }
}

/// A small badge showing the resolved effective decision + its source.
fn effective_badge(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, at: Point, decision: Decision, source: &Source) {
    let c = &theme.tokens.colors;
    let (label, col) = match decision {
        Decision::Allow => ("ALLOW", c.semantic.success),
        Decision::Deny => ("DENY", c.semantic.danger),
    };
    let w = 76.0;
    let r = Rect::new(at.x, at.y, at.x + w, at.y + 18.0);
    let sh = RoundedRect::from_rect(r, 4.0);
    scene.fill(Fill::NonZero, t, col.with_alpha_factor(0.18), None, &sh);
    tr.draw_text_weighted(scene, label, 9.0, col, t * Affine::translate((r.x0 + 6.0, r.y0 + 3.0)), None, TextWeight::Bold);
    let src = match source {
        Source::MemberOverride => "ovr",
        Source::Role(_) => "role",
        Source::Default => "def",
    };
    tr.draw_text(scene, src, 9.0, c.text_dim, t * Affine::translate((r.x0 + 44.0, r.y0 + 3.0)), None);
}

fn menu_panel(scene: &mut Scene, t: Affine, theme: &Theme, panel: Rect) {
    let c = &theme.tokens.colors;
    let shadow = RoundedRect::from_rect(panel.inset(2.0), theme.tokens.radii.md as f64 + 1.0);
    scene.fill(Fill::NonZero, t, Color::from_rgba8(0, 0, 0, 90), None, &shadow);
    let shape = RoundedRect::from_rect(panel, theme.tokens.radii.md as f64);
    scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &shape);
    scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
}

#[allow(clippy::too_many_arguments)]
fn menu_item(scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, rect: Rect, label: &str, hover: bool, selected: bool) {
    let c = &theme.tokens.colors;
    if hover {
        scene.fill(Fill::NonZero, t, theme.accent_primary, None, &RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64));
    } else if selected {
        scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &RoundedRect::from_rect(rect, theme.tokens.radii.sm as f64));
    }
    let col = if hover { Color::WHITE } else { c.text_primary };
    tr.draw_text(scene, label, 13.0, col, t * Affine::translate((rect.x0 + 12.0, rect.center().y - 8.0)), Some((rect.width() - 20.0) as f32));
}

fn color_grid_geometry(anchor: Rect) -> (Rect, Vec<Rect>) {
    let cols = 4;
    let cell = 30.0;
    let pad = 8.0;
    let rows = ROLE_PALETTE.len().div_ceil(cols);
    let w = cols as f64 * cell + pad * 2.0;
    let h = rows as f64 * cell + pad * 2.0;
    let panel = Rect::new(anchor.x0, anchor.y1 + 4.0, anchor.x0 + w, anchor.y1 + 4.0 + h);
    let mut swatches = Vec::new();
    for i in 0..ROLE_PALETTE.len() {
        let r = (i / cols) as f64;
        let col = (i % cols) as f64;
        let x = panel.x0 + pad + col * cell;
        let y = panel.y0 + pad + r * cell;
        swatches.push(Rect::new(x + 2.0, y + 2.0, x + cell - 2.0, y + cell - 2.0));
    }
    (panel, swatches)
}

/// A fixed palette the color picker offers and `create_role` cycles
/// through.
const ROLE_PALETTE: [u32; 12] = [
    0xE6_4A_4A, 0xE6_8A_3A, 0xE6_C84A, 0x6FC74A, 0x4AC77A, 0x4AC7B8, 0x4A8FE6, 0x6A5AE6, 0x9A4AE6,
    0xE64AB8, 0x9A9A9A, 0x5A6472,
];

fn sample_color(i: usize) -> u32 {
    ROLE_PALETTE[i % ROLE_PALETTE.len()]
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use taffy::{AvailableSpace, Dimension, Size, Style};

    fn drive_frame(view: &mut EditorView, w: u32, h: u32) {
        let theme = Theme::dark();
        let presence = PresenceCtx::empty();
        let permission = PermissionContext::permissive();
        let mut tr = TextRenderer::new();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let mut doc = AutoCommit::new();
        view.sync(&mut doc, &ROOT);
        let child = view.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() },
                &[child],
            )
            .unwrap();
        {
            let mm = node_map.clone();
            let trr = &mut tr;
            taffy
                .compute_layout_with_measure(
                    root,
                    Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) },
                    |k, a, n, _, _| if let Some(&p) = mm.get(&n) { unsafe { (*p).measure(k, a, trr) } } else { taffy::Size::ZERO },
                )
                .unwrap();
        }
        view.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        view.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
        let _ = &scene;
    }

    #[test]
    fn seeds_sample_set_on_first_sync() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        assert!(!v.set().roles.is_empty(), "sample roles seeded");
        assert!(v.selected_role.is_some(), "a role auto-selected");
    }

    #[test]
    fn roles_frame_does_not_panic() {
        let mut v = EditorView::new();
        drive_frame(&mut v, 1280, 800);
    }

    #[test]
    fn members_frame_does_not_panic() {
        let mut v = EditorView::from_intent(EditorIntent::members(None));
        drive_frame(&mut v, 1280, 800);
    }

    #[test]
    fn dynamic_context_frame_does_not_panic() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        v.active_context = Some(ContextId::new("chat:server:eng"));
        drive_frame(&mut v, 1280, 800);
        // The grid should now contain only per-server permissions.
        let perms = v.grid_permissions();
        assert!(perms.iter().all(|d| d.scope.is_dynamic()), "only dynamic perms in a context");
        assert!(perms.iter().any(|d| d.id == "chat.manage_channels"));
    }

    #[test]
    fn create_role_adds_and_selects() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        let before = v.set().roles.len();
        v.create_role();
        assert_eq!(v.set().roles.len(), before + 1);
        // The new role is selected and on top of the hierarchy.
        let sel = v.selected_role.unwrap();
        let role = v.set().role(sel).unwrap();
        assert_eq!(role.position, v.set().roles.iter().map(|r| r.position).max().unwrap());
    }

    #[test]
    fn set_grant_direct_persists_to_doc() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        let rid = v.selected_role.unwrap();
        v.set_grant_direct("workspace.manage", Grant::Deny);
        v.flush(&mut doc);
        let back = PermissionsDoc::read(&doc);
        let r = back.role(rid).unwrap();
        assert_eq!(r.grant_for("workspace.manage", None), Grant::Deny);
    }

    #[test]
    fn search_filters_the_grid() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        v.search.set_text("invite");
        let perms = v.grid_permissions();
        assert!(perms.iter().any(|d| d.id == "workspace.invite"));
        assert!(!perms.iter().any(|d| d.id == "chat.send_message"));
    }

    #[test]
    fn reorder_swaps_hierarchy_positions() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        // Select the lowest role, push it up, and confirm its position rose.
        let lowest = v.set().roles.iter().min_by_key(|r| r.position).unwrap().id;
        let before = v.set().role(lowest).unwrap().position;
        v.selected_role = Some(lowest);
        v.reorder_selected(1);
        let after = v.set().role(lowest).unwrap().position;
        assert!(after > before, "reorder up must raise position");
    }
}
