//! The detail pane — NavSplit's *content* child. Renders the selected
//! role's allow/deny/inherit grid, or the selected member's role
//! checkboxes + override grid with the live effective-permission preview.
//!
//! It owns the search box (which filters the grid) and the per-instance
//! context picker + color picker popups. All state changes flow through
//! the shared [`EditorState`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Color, Fill};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{TextInput, Widget};
use ui::ColorCompat;

use super::draw::{
    color_grid_geometry, danger_button, effective_badge, menu_item, menu_panel, packed_color,
    role_chip, seg_caption, tri_state, Sizes, SEG_W,
};
use super::shared::{EditorState, EditorTab, Shared, ROLE_PALETTE};
use crate::check::explain;
use crate::model::{ContextId, Grant, Member, Scope};
use crate::registry::PermissionDef;

const PAD: f64 = 16.0;
const HEADER_H: f64 = 96.0;
const GRID_ROW_H: f64 = 40.0;

enum Popup {
    ContextPicker,
    ColorPicker { anchor: Rect },
}

/// Identity of the inputs that determine the grid's *derived data* — the
/// filtered + scope-grouped permission list. When this is unchanged between
/// frames the cached `groups` are reused as-is instead of re-running
/// `registry.search` + the scope filter + grouping (the expensive part).
///
/// This mirrors the calculator's plot-cache key (`graph2d::CurveKey`):
/// value-typed, derived fresh each frame, compared for equality. It keys on
/// the search query and the active context — the ONLY inputs that change
/// *which permissions appear* in the grid. It deliberately EXCLUDES:
///   * the **theme** (re-theming replays the cached groups recoloured),
///   * the **grid rect** (a resize re-derives only the cell *rects*, cheap),
///   * the **selected role/member** and the **tab** (both views show the
///     same global filtered set; the per-role grant / per-member effective
///     value is looked up at draw time from the cached group list — so
///     switching role or tab must NOT rebuild this).
///
/// `rev` is included so a *mutation* (a grant flip, a role rename, a new
/// permission registered, an identity-roster change) invalidates the cache,
/// while pure navigation (select / tab) does not. (`rev` is bumped on those
/// too, but the query/context are what gate the heavy rebuild — see the
/// `tab_and_role_switches_do_not_rebuild_grid_state` test.)
#[derive(Clone, PartialEq, Default)]
struct GridKey {
    /// Registry generation — a mutation that could change the *set* (not just
    /// a grant value) must rebuild. We approximate "set changed" with the
    /// registry length + the active context + query, which together pin the
    /// filtered list. The registry is fixed for the editor's lifetime today,
    /// so in practice query + context are the live inputs.
    reg_len: usize,
    query: String,
    context: Option<ContextId>,
}

/// Cache of the derived grid data, gated by [`GridKey`] equality.
#[derive(Default)]
struct GridCache {
    key: Option<GridKey>,
    /// The filtered, scope-grouped permission list both `layout_*` and
    /// `draw_*` consume. Recomputing this every frame (search + filter +
    /// clone + group) was the per-switch lag.
    groups: Vec<(Scope, Vec<PermissionDef>)>,
    /// Real-rebuild counter — test observability that a switch did NOT
    /// rebuild expensive state (mirrors `CurveCache::rebuilds`).
    rebuilds: u32,
}

pub struct DetailPane {
    shared: Shared,
    search: TextInput,
    popup: Option<Popup>,

    rect: Rect,
    detail_scroll: f64,
    content_h: f64,
    last_mouse: Point,

    // hit rects
    context_chip: Rect,
    role_swatch: Rect,
    role_delete: Rect,
    grid_cells: Vec<(String, Grant, Rect)>,
    role_checks: Vec<(crate::model::RoleId, Rect)>,
    override_cells: Vec<(String, Grant, Rect)>,
    context_rows: Vec<(Option<ContextId>, Rect)>,

    /// Memoised derived grid data (filtered + scope-grouped perms). Only
    /// rebuilt when [`GridKey`] changes — see [`Self::ensure_grid`].
    grid_cache: GridCache,

    dirty: bool,
    relayout: bool,
}

impl DetailPane {
    pub fn new(shared: Shared) -> Self {
        DetailPane {
            shared,
            search: TextInput::local().with_id("permissions_search").with_placeholder("Search permissions\u{2026}"),
            popup: None,
            rect: Rect::ZERO,
            detail_scroll: 0.0,
            content_h: 0.0,
            last_mouse: Point::ZERO,
            context_chip: Rect::ZERO,
            role_swatch: Rect::ZERO,
            role_delete: Rect::ZERO,
            grid_cells: Vec::new(),
            role_checks: Vec::new(),
            override_cells: Vec::new(),
            context_rows: Vec::new(),
            grid_cache: GridCache::default(),
            dirty: true,
            relayout: true,
        }
    }

    fn tab(&self) -> EditorTab {
        self.shared.borrow().tab
    }

    /// Rebuild the cached filtered + scope-grouped permission list **only
    /// when the grid key changed** (revision / query / context / tab). On an
    /// unchanged key this is a cheap early-return, so tab- and role-switches
    /// — which keep the same filtered set — never re-run the search/filter/
    /// group pipeline. Call before reading [`Self::grid_groups`].
    fn ensure_grid(&mut self) {
        let key = {
            let s = self.shared.borrow();
            GridKey {
                reg_len: s.registry.len(),
                query: self.search.value().to_string(),
                context: s.active_context.clone(),
            }
        };
        if self.grid_cache.key.as_ref() == Some(&key) {
            return; // HIT — reuse cached groups, no recompute.
        }
        let perms = {
            let s = self.shared.borrow();
            s.grid_permissions(&key.query)
        };
        self.grid_cache.groups = EditorState::group_by_scope(&perms);
        self.grid_cache.key = Some(key);
        self.grid_cache.rebuilds = self.grid_cache.rebuilds.wrapping_add(1);
    }

    fn layout(&mut self) {
        // Refresh the cached filtered/grouped grid (no-op unless the key
        // moved), then place cells from it. The expensive derive happens at
        // most once per real change, not per layout pass.
        self.ensure_grid();
        self.grid_cells.clear();
        self.role_checks.clear();
        self.override_cells.clear();
        let area = self.rect;
        let x1 = area.x1 - PAD;

        // Header band: context chip (top-right) + search box.
        self.context_chip = Rect::new(x1 - 200.0, area.y0 + 14.0, x1, area.y0 + 14.0 + 32.0);
        let sx = area.x0 + PAD;
        let sy = area.y0 + 54.0;
        let search_rect = Rect::new(sx, sy, area.x1 - PAD, sy + 34.0);
        self.search.apply_layout_external(search_rect);

        match self.tab() {
            EditorTab::Roles => self.layout_role(),
            EditorTab::Members => self.layout_member(),
        }
    }

    fn layout_role(&mut self) {
        let area = self.rect;
        let x1 = area.x1 - PAD;
        self.role_swatch = Rect::new(area.x0 + PAD, area.y0 + 14.0, area.x0 + PAD + 28.0, area.y0 + 42.0);
        self.role_delete = Rect::new(x1 - 84.0, area.y0 + 14.0, x1, area.y0 + 44.0);

        let groups = self.grid_cache.groups.clone();
        let seg_x = x1 - SEG_W * 3.0;
        let mut y = area.y0 + HEADER_H - self.detail_scroll;
        for (_scope, list) in &groups {
            y += 32.0;
            for d in list {
                let cell = |i: usize| Rect::new(seg_x + i as f64 * SEG_W, y, seg_x + (i as f64 + 1.0) * SEG_W, y + GRID_ROW_H - 6.0);
                self.grid_cells.push((d.id.clone(), Grant::Allow, cell(0)));
                self.grid_cells.push((d.id.clone(), Grant::Inherit, cell(1)));
                self.grid_cells.push((d.id.clone(), Grant::Deny, cell(2)));
                y += GRID_ROW_H;
            }
            y += 10.0;
        }
        self.content_h = (y + self.detail_scroll) - (area.y0 + HEADER_H) + 48.0;
    }

    fn layout_member(&mut self) {
        let area = self.rect;
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let mut y = area.y0 + HEADER_H - self.detail_scroll;

        // Role checkbox chips.
        y += 28.0; // "ROLES" header
        let roles = self.shared.borrow().visible_roles();
        let mut cx = x0;
        let chip_w = 168.0;
        for rid in &roles {
            if cx + chip_w > x1 {
                cx = x0;
                y += 40.0;
            }
            self.role_checks.push((*rid, Rect::new(cx, y, cx + chip_w, y + 34.0)));
            cx += chip_w + 8.0;
        }
        y += 34.0 + 22.0;

        // Override grid.
        y += 28.0; // "OVERRIDES + EFFECTIVE" header
        let groups = self.grid_cache.groups.clone();
        let seg_x = x1 - SEG_W * 3.0;
        for (_scope, list) in &groups {
            y += 30.0;
            for d in list {
                let cell = |i: usize| Rect::new(seg_x + i as f64 * SEG_W, y, seg_x + (i as f64 + 1.0) * SEG_W, y + GRID_ROW_H - 6.0);
                self.override_cells.push((d.id.clone(), Grant::Allow, cell(0)));
                self.override_cells.push((d.id.clone(), Grant::Inherit, cell(1)));
                self.override_cells.push((d.id.clone(), Grant::Deny, cell(2)));
                y += GRID_ROW_H;
            }
            y += 10.0;
        }
        self.content_h = (y + self.detail_scroll) - (area.y0 + HEADER_H) + 48.0;
    }

    // ---- drawing ----------------------------------------------------

    fn draw_pane(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);
        match self.tab() {
            EditorTab::Roles => self.draw_role(scene, t, tr, theme, presence),
            EditorTab::Members => self.draw_member(scene, t, tr, theme),
        }
        if let Some(popup) = &self.popup {
            self.draw_popup(scene, t, tr, theme, popup);
        }
    }

    fn draw_context_chip(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let s = self.shared.borrow();
        let (label, active) = match &s.active_context {
            Some(id) => (s.set.context(id).map(|cc| format!("\u{f0c1} {}", cc.label)).unwrap_or_else(|| id.0.clone()), true),
            None => ("\u{f0ac} Global".to_string(), false),
        };
        let rect = self.context_chip;
        let shape = RoundedRect::from_rect(rect, theme.tokens.radii.md as f64);
        let bg = if active { theme.accent_primary.with_alpha_factor(0.16) } else { c.surfaces.raised };
        scene.fill(Fill::NonZero, t, bg, None, &shape);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        let col = if active { theme.accent_primary } else { c.text_primary };
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &shape);
        tr.draw_text(scene, &label, sz.secondary, col, t * Affine::translate((rect.x0 + 10.0, rect.center().y - sz.secondary as f64 / 2.0)), Some((rect.width() - 16.0) as f32));
        scene.pop_layer();
    }

    fn draw_role(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let area = self.rect;
        let (role, has_sel) = {
            let s = self.shared.borrow();
            (s.selected_role.and_then(|id| s.set.role(id).cloned()), s.selected_role.is_some())
        };
        let Some(role) = role else {
            let _ = has_sel;
            tr.draw_text(scene, "Select or create a role", sz.body, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 40.0)), None);
            self.draw_context_chip(scene, t, tr, theme);
            return;
        };

        // Header: swatch + name + position.
        let sw = RoundedRect::from_rect(self.role_swatch, 6.0);
        scene.fill(Fill::NonZero, t, packed_color(role.color), None, &sw);
        tr.draw_text_weighted(scene, &role.name, sz.title, c.text_primary, t * Affine::translate((self.role_swatch.x1 + 12.0, area.y0 + 14.0)), None, TextWeight::Bold);
        tr.draw_text(scene, &format!("precedence {}", role.position), sz.caption, c.text_dim, t * Affine::translate((self.role_swatch.x1 + 12.0, area.y0 + 14.0 + sz.title as f64 + 2.0)), None);

        danger_button(scene, t, tr, theme, self.role_delete, "Delete");
        self.draw_context_chip(scene, t, tr, theme);

        // Search box.
        self.search.draw(scene, t, tr, theme, presence);

        // Grid — replayed from the cached, scope-grouped permission list
        // (rebuilt only when the grid key moves; see `ensure_grid`).
        let groups = self.grid_cache.groups.clone();
        let active_ctx = self.shared.borrow().active_context.clone();
        self.draw_grid(scene, t, tr, theme, &groups, |perm| role.grant_for(perm, active_ctx.as_ref()));
    }

    fn draw_member(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let area = self.rect;
        let member = {
            let s = self.shared.borrow();
            s.selected_member.and_then(|id| s.set.member(id).cloned())
        };
        let Some(member) = member else {
            tr.draw_text(scene, "Select or create a member", sz.body, c.text_dim, t * Affine::translate((area.x0 + PAD, area.y0 + 40.0)), None);
            self.draw_context_chip(scene, t, tr, theme);
            return;
        };

        // Header resolves through the REAL identity layer: the display name,
        // colour, and initials come from the bound qOS identity (the same
        // peer the collaboration roster knows), not the member's frozen
        // label. An avatar chip (initials on the identity colour) stands in
        // for the absent avatar image, matching that peer's presence colour.
        let (disp_name, ident_color, initials, ident_sub) = {
            let s = self.shared.borrow();
            let name = s.member_display_name(&member);
            let color = s.member_color(&member);
            let inits = member
                .identity_id
                .as_deref()
                .and_then(|id| s.identities.get(id))
                .map(|i| i.initials())
                .unwrap_or_else(|| crate::identity::Identity::new("", &name).initials());
            let sub = match &member.identity_id {
                Some(id) => format!("identity: {id}"),
                None => "unlinked member".to_string(),
            };
            (name, color, inits, sub)
        };
        // Avatar chip.
        let av = Rect::new(area.x0 + PAD, area.y0 + 14.0, area.x0 + PAD + 34.0, area.y0 + 48.0);
        let av_sh = RoundedRect::from_rect(av, theme.tokens.radii.md as f64);
        scene.fill(Fill::NonZero, t, packed_color(ident_color), None, &av_sh);
        let iw = tr.measure(&initials, sz.secondary, None).0 as f64;
        tr.draw_text_weighted(scene, &initials, sz.secondary, Color::WHITE, t * Affine::translate((av.center().x - iw / 2.0, av.center().y - sz.secondary as f64 / 2.0)), None, TextWeight::Bold);
        let name_x = av.x1 + 12.0;
        tr.draw_text_weighted(scene, &disp_name, sz.title, c.text_primary, t * Affine::translate((name_x, area.y0 + 12.0)), None, TextWeight::Bold);
        let role_names: Vec<String> = self.shared.borrow().set.roles_of(&member).iter().map(|r| r.name.clone()).collect();
        let roles_sub = if role_names.is_empty() { "no roles".to_string() } else { role_names.join(", ") };
        let sub = format!("{ident_sub}  \u{00b7}  {roles_sub}");
        tr.draw_text(scene, &sub, sz.secondary, c.text_dim, t * Affine::translate((name_x, area.y0 + 14.0 + sz.title as f64 + 2.0)), Some((area.width() - (name_x - area.x0) - PAD) as f32));

        self.draw_context_chip(scene, t, tr, theme);

        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &Rect::new(area.x0, area.y0 + HEADER_H - 4.0, area.x1, area.y1));

        // ROLES header.
        let roles_hdr_y = area.y0 + HEADER_H - self.detail_scroll + 6.0;
        tr.draw_text_weighted(scene, "ROLES", sz.caption, c.text_dim, t * Affine::translate((area.x0 + PAD, roles_hdr_y)), None, TextWeight::Semibold);
        let checks = self.role_checks.clone();
        for (rid, rect) in &checks {
            let Some(role) = self.shared.borrow().set.role(*rid).cloned() else { continue };
            let held = member.has_role(*rid);
            role_chip(scene, t, tr, theme, *rect, &role.name, packed_color(role.color), held);
        }

        // OVERRIDES + EFFECTIVE — replayed from the cached grouped list.
        let groups = self.grid_cache.groups.clone();
        let ctx = self.shared.borrow().active_context.clone();
        self.draw_override_grid(scene, t, tr, theme, &groups, &member, ctx.as_ref());

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
    ) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let area = self.rect;
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &Rect::new(area.x0, area.y0 + HEADER_H - 4.0, area.x1, area.y1));
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let seg_x = x1 - SEG_W * 3.0;
        let mut y = area.y0 + HEADER_H - self.detail_scroll;
        if groups.is_empty() {
            tr.draw_text(scene, "No permissions match.", sz.body, c.text_dim, t * Affine::translate((x0, y + 10.0)), None);
        }
        for (scope, list) in groups {
            tr.draw_text_weighted(scene, &scope.label().to_uppercase(), sz.caption, theme.accent_primary, t * Affine::translate((x0, y + 12.0)), None, TextWeight::Semibold);
            seg_caption(scene, t, tr, theme, seg_x, y + 10.0);
            y += 32.0;
            for d in list {
                let g = value_of(&d.id);
                tr.draw_text(scene, &d.label, sz.body, c.text_primary, t * Affine::translate((x0, y + 3.0)), Some((seg_x - x0 - 12.0) as f32));
                tr.draw_text(scene, &d.description, sz.secondary, c.text_dim, t * Affine::translate((x0, y + 3.0 + sz.body as f64 + 3.0)), Some((seg_x - x0 - 12.0) as f32));
                tri_state(scene, t, tr, theme, seg_x, y, g, GRID_ROW_H);
                y += GRID_ROW_H;
            }
            y += 10.0;
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
        member: &Member,
        ctx: Option<&ContextId>,
    ) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let area = self.rect;
        let x0 = area.x0 + PAD;
        let x1 = area.x1 - PAD;
        let seg_x = x1 - SEG_W * 3.0;
        let s = self.shared.borrow();
        let cell_y = |perm: &str| -> Option<f64> {
            self.override_cells.iter().find(|(p, g, _)| p == perm && *g == Grant::Allow).map(|(_, _, r)| r.y0)
        };
        if let Some(first) = self.override_cells.first().map(|(_, _, r)| r.y0) {
            let hy = first - 30.0;
            tr.draw_text_weighted(scene, "OVERRIDES  +  EFFECTIVE", sz.caption, c.text_dim, t * Affine::translate((x0, hy)), None, TextWeight::Semibold);
            seg_caption(scene, t, tr, theme, seg_x, hy);
        }
        for (scope, list) in groups {
            if let Some(first_perm) = list.first() {
                if let Some(y) = cell_y(&first_perm.id) {
                    tr.draw_text_weighted(scene, &scope.label().to_uppercase(), sz.caption, theme.accent_primary, t * Affine::translate((x0, y - 18.0)), None, TextWeight::Semibold);
                }
            }
            for d in list {
                let Some(y) = cell_y(&d.id) else { continue };
                let ov = member.override_for(&d.id, ctx);
                let res = explain(&s.registry, &s.set, member, &d.id, ctx);
                tr.draw_text(scene, &d.label, sz.body, c.text_primary, t * Affine::translate((x0, y + 4.0)), Some((seg_x - x0 - 100.0) as f32));
                effective_badge(scene, t, tr, theme, Point::new(seg_x - 96.0, y + 6.0), res.decision, &res.source);
                tri_state(scene, t, tr, theme, seg_x, y, ov, GRID_ROW_H);
            }
        }
    }

    fn draw_popup(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, popup: &Popup) {
        match popup {
            Popup::ContextPicker => {
                let rows = &self.context_rows;
                if rows.is_empty() {
                    return;
                }
                let first = rows[0].1;
                let last = rows.last().unwrap().1;
                let panel = Rect::new(first.x0 - 6.0, first.y0 - 6.0, first.x1 + 6.0, last.y1 + 6.0);
                menu_panel(scene, t, theme, panel);
                let s = self.shared.borrow();
                for (ctx, rect) in rows {
                    let label = match ctx {
                        None => "Global".to_string(),
                        Some(id) => s.set.context(id).map(|cc| cc.label.clone()).unwrap_or_else(|| id.0.clone()),
                    };
                    let hov = rect.contains(self.last_mouse);
                    let sel = &s.active_context == ctx;
                    menu_item(scene, t, tr, theme, *rect, &label, hov, sel);
                }
            }
            Popup::ColorPicker { anchor } => {
                let (panel, swatches) = color_grid_geometry(*anchor, ROLE_PALETTE.len());
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

    fn open_context_picker(&mut self) {
        let anchor = self.context_chip;
        let item_h = 34.0;
        let mut rows = vec![(None, Rect::new(anchor.x0, anchor.y1 + 4.0, anchor.x1, anchor.y1 + 4.0 + item_h))];
        let mut y = anchor.y1 + 4.0 + item_h;
        for ctx in &self.shared.borrow().set.contexts {
            rows.push((Some(ctx.id.clone()), Rect::new(anchor.x0, y, anchor.x1, y + item_h)));
            y += item_h;
        }
        self.context_rows = rows;
        self.popup = Some(Popup::ContextPicker);
        self.dirty = true;
    }

    fn set_grant_direct(&mut self, perm: &str, target: Grant) {
        let mut s = self.shared.borrow_mut();
        let ctx = s.active_context.clone();
        if let Some(id) = s.selected_role {
            if let Some(role) = s.set.role_mut(id) {
                role.set_grant(perm, ctx, target);
                s.mark_changed();
            }
        }
    }

    fn set_override_direct(&mut self, perm: &str, target: Grant) {
        let mut s = self.shared.borrow_mut();
        let ctx = s.active_context.clone();
        if let Some(id) = s.selected_member {
            if let Some(m) = s.set.member_mut(id) {
                m.set_override(perm, ctx, target);
                s.mark_changed();
            }
        }
    }
}

impl Widget for DetailPane {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.search.sync(state, path);
    }

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy
            .new_leaf(Style { size: Size { width: Dimension::percent(1.0), height: Dimension::percent(1.0) }, ..Default::default() })
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
        self.draw_pane(scene, transform, tr, theme, presence);
    }

    fn draw_with_presence(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, _permissions: &ui::permissions::PermissionContext) {
        self.draw_pane(scene, transform, tr, theme, presence);
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        state: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_mouse = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // Popups consume first.
        if self.popup.is_some() {
            self.handle_popup(event, handled);
            if *handled {
                return;
            }
        }

        // Pointer presses + wheel are only ours if they land inside our own
        // content rect. NavSplit routes the content pane FIRST when docked,
        // so without this guard the detail pane would swallow EVERY click in
        // the editor — including clicks in the sidebar column — and mark them
        // handled, starving the sidebar of its row clicks. That was the real
        // "clicking another role doesn't switch" bug: the press never reached
        // the sidebar. We only claim pointer presses within `self.rect`.
        let pointer_inside = self.rect.contains(self.last_mouse);

        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                if !pointer_inside {
                    return; // not ours — let it fall through to the sidebar.
                }
                self.handle_click(state, path, handled, node_id, taffy, modifiers);
                return;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                if !pointer_inside {
                    return;
                }
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => (-y * 48.0) as f64,
                    MouseScrollDelta::PixelDelta(p) => -p.y,
                };
                let max = (self.content_h - self.rect.height() + HEADER_H + 40.0).max(0.0);
                self.detail_scroll = (self.detail_scroll + dy).clamp(0.0, max);
                self.relayout = true;
                self.dirty = true;
                *handled = true;
                return;
            }
            WindowEvent::KeyboardInput { event: ke, .. } => {
                self.handle_key(ke, state, path, handled, node_id, taffy, modifiers, event, scale_factor);
                return;
            }
            _ => {}
        }

        // Forward to search for hover.
        let mut sink = None;
        self.search.handle_event(event, scale_factor, modifiers, state, path, handled, &mut sink, node_id, taffy);
        let _ = focus_target;
    }

    fn name(&self) -> &str {
        "PermissionsDetail"
    }
    fn test_id(&self) -> Option<&str> {
        Some("PermissionsDetail")
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

impl DetailPane {
    #[allow(clippy::too_many_arguments)]
    fn handle_click(&mut self, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, node_id: NodeId, taffy: &TaffyTree<()>, modifiers: &winit::event::Modifiers) {
        let p = self.last_mouse;

        if self.context_chip.contains(p) {
            self.open_context_picker();
            *handled = true;
            return;
        }

        if self.tab() == EditorTab::Roles {
            if self.role_swatch.contains(p) && self.shared.borrow().selected_role.is_some() {
                self.popup = Some(Popup::ColorPicker { anchor: self.role_swatch });
                self.dirty = true;
                *handled = true;
                return;
            }
            if self.role_delete.contains(p) {
                if let Some(id) = self.shared.borrow().selected_role {
                    self.shared.borrow_mut().delete_role(id);
                }
                self.relayout = true;
                self.dirty = true;
                *handled = true;
                return;
            }
            if let Some((perm, seg, _)) = self.grid_cells.iter().find(|(_, _, r)| r.contains(p)).cloned() {
                let current = {
                    let s = self.shared.borrow();
                    s.selected_role.and_then(|id| s.set.role(id)).map(|r| r.grant_for(&perm, s.active_context.as_ref())).unwrap_or(Grant::Inherit)
                };
                let target = if current == seg { Grant::Inherit } else { seg };
                self.set_grant_direct(&perm, target);
                *handled = true;
                return;
            }
        }

        if self.tab() == EditorTab::Members {
            if let Some((rid, _)) = self.role_checks.iter().find(|(_, r)| r.contains(p)).cloned() {
                let mid = self.shared.borrow().selected_member;
                if let Some(mid) = mid {
                    self.shared.borrow_mut().toggle_member_role(mid, rid);
                }
                self.relayout = true;
                self.dirty = true;
                *handled = true;
                return;
            }
            if let Some((perm, seg, _)) = self.override_cells.iter().find(|(_, _, r)| r.contains(p)).cloned() {
                let current = {
                    let s = self.shared.borrow();
                    s.selected_member.and_then(|id| s.set.member(id)).map(|m| m.override_for(&perm, s.active_context.as_ref())).unwrap_or(Grant::Inherit)
                };
                let target = if current == seg { Grant::Inherit } else { seg };
                self.set_override_direct(&perm, target);
                *handled = true;
                return;
            }
        }

        // Forward to the search box for caret placement / focus.
        let press = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left };
        let mut sink = None;
        let mut h = false;
        self.search.set_focus(self.search.get_rect().contains(p));
        if self.search.is_focused() {
            self.search.handle_event(&press, 1.0, modifiers, state, path, &mut h, &mut sink, node_id, taffy);
        }
        self.dirty = true;
        *handled = true;
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
                if let Some((ctx, _)) = self.context_rows.iter().find(|(_, r)| r.contains(p)).cloned() {
                    // Route through `set_active_context` so the grid cache
                    // invalidates for the new scope.
                    self.shared.borrow_mut().set_active_context(ctx);
                    self.detail_scroll = 0.0;
                }
                self.context_rows.clear();
            }
            Popup::ColorPicker { anchor } => {
                let (_panel, swatches) = color_grid_geometry(anchor, ROLE_PALETTE.len());
                if let Some(i) = swatches.iter().position(|r| r.contains(p)) {
                    let sel = self.shared.borrow().selected_role;
                    if let Some(id) = sel {
                        if let Some(role) = self.shared.borrow_mut().set.role_mut(id) {
                            role.color = ROLE_PALETTE[i];
                        }
                        self.shared.borrow_mut().mark_changed();
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
        if self.search.is_focused() {
            let mut sink = None;
            self.search.handle_event(full_event, scale_factor, modifiers, state, path, handled, &mut sink, node_id, taffy);
            self.relayout = true;
            self.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;
    use automerge::ROOT;
    use taffy::AvailableSpace;

    fn shared() -> Shared {
        let s = EditorState::new(sample::sample_registry(), sample::sample_set());
        s.borrow_mut().ensure_selection();
        s
    }

    fn laid_out(pane: &mut DetailPane, w: f64, h: f64) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() }, &[child])
            .unwrap();
        taffy.compute_layout(root, Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) }).unwrap();
        pane.apply_layout(&taffy, child, Point::new(0.0, 0.0));
        (taffy, child)
    }

    fn click(pane: &mut DetailPane, taffy: &TaffyTree<()>, node: NodeId, p: Point) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        let mv = WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(p.x, p.y) };
        pane.handle_event(&mv, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
        let ev = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left };
        pane.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
    }

    /// PERF: re-laying-out an unchanged grid must NOT rebuild the cached
    /// filtered/grouped permission list. The first layout builds it once;
    /// subsequent frames with the same state replay the cache.
    #[test]
    fn repeated_frames_do_not_rebuild_the_grid_cache() {
        let mut pane = DetailPane::new(shared());
        laid_out(&mut pane, 1000.0, 800.0);
        let after_first = pane.grid_cache.rebuilds;
        assert_eq!(after_first, 1, "first layout builds the grid once");
        // Several more frames with no state change.
        for _ in 0..5 {
            laid_out(&mut pane, 1000.0, 800.0);
        }
        assert_eq!(pane.grid_cache.rebuilds, after_first, "unchanged frames replay the cache — no rebuild");
    }

    /// PERF: switching Members↔Roles (and switching roles) does NOT rebuild
    /// the expensive grid state — both tabs show the same global filtered
    /// permission set, and a role switch keeps the same set. The cache key
    /// excludes the selected role; only query/context change the set. This
    /// is the structural fix that makes switching feel instant.
    #[test]
    fn tab_and_role_switches_do_not_rebuild_grid_state() {
        let s = shared();
        let mut pane = DetailPane::new(s.clone());
        laid_out(&mut pane, 1000.0, 800.0);
        let baseline = pane.grid_cache.rebuilds;
        assert_eq!(baseline, 1);

        // Flip to Members and back several times, re-laying-out each time
        // (what the App does on a handled tab click).
        for _ in 0..4 {
            s.borrow_mut().select_tab(EditorTab::Members);
            laid_out(&mut pane, 1000.0, 800.0);
            s.borrow_mut().select_tab(EditorTab::Roles);
            laid_out(&mut pane, 1000.0, 800.0);
        }
        // Switch among roles.
        let roles = s.borrow().visible_roles();
        for rid in &roles {
            s.borrow_mut().select_role(*rid);
            laid_out(&mut pane, 1000.0, 800.0);
        }
        assert_eq!(
            pane.grid_cache.rebuilds, baseline,
            "tab + role switches must NOT rebuild the filtered/grouped grid (it's the same set) — rebuilds went {baseline} -> {}",
            pane.grid_cache.rebuilds
        );

        // A query change SHOULD rebuild (proves the cache isn't just frozen).
        pane.search.set_text("call");
        laid_out(&mut pane, 1000.0, 800.0);
        assert!(pane.grid_cache.rebuilds > baseline, "a search-query change rebuilds the grid");
    }

    /// PERF: changing the active context rebuilds (different permission set),
    /// confirming the cache invalidates on the inputs that truly change it.
    #[test]
    fn context_change_invalidates_grid_cache() {
        let s = shared();
        let ctx = s.borrow().set.contexts.first().map(|c| c.id.clone());
        let Some(ctx) = ctx else { return };
        let mut pane = DetailPane::new(s.clone());
        laid_out(&mut pane, 1000.0, 800.0);
        let before = pane.grid_cache.rebuilds;
        s.borrow_mut().set_active_context(Some(ctx));
        laid_out(&mut pane, 1000.0, 800.0);
        assert!(pane.grid_cache.rebuilds > before, "switching context rebuilds for the new per-instance set");
    }

    #[test]
    fn clicking_a_grid_deny_cell_sets_the_grant() {
        let mut pane = DetailPane::new(shared());
        let (taffy, node) = laid_out(&mut pane, 1000.0, 800.0);
        // Find the Deny cell for some permission in the grid.
        let (perm, _seg, rect) = pane.grid_cells.iter().find(|(_, g, _)| *g == Grant::Deny).cloned().unwrap();
        click(&mut pane, &taffy, node, rect.center());
        let rid = pane.shared.borrow().selected_role.unwrap();
        assert_eq!(pane.shared.borrow().set.role(rid).unwrap().grant_for(&perm, None), Grant::Deny);
    }

    #[test]
    fn toggling_a_member_role_checkbox_assigns_it() {
        let s = shared();
        s.borrow_mut().tab = EditorTab::Members;
        let mut pane = DetailPane::new(s);
        let (taffy, node) = laid_out(&mut pane, 1000.0, 800.0);
        let (rid, rect) = pane.role_checks[0];
        let mid = pane.shared.borrow().selected_member.unwrap();
        let had = pane.shared.borrow().set.member(mid).unwrap().has_role(rid);
        click(&mut pane, &taffy, node, rect.center());
        let now = pane.shared.borrow().set.member(mid).unwrap().has_role(rid);
        assert_ne!(had, now, "clicking the role chip toggled the assignment");
    }
}
