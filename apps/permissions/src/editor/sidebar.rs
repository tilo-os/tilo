//! The sidebar pane — the roles / members list that NavSplit docks (wide)
//! or floats as an overlay flap (narrow).
//!
//! It owns the interactions the live review called for:
//!   * **Select** a row (single click).
//!   * **Rename** a role inline: click/double-click the selected role's
//!     name, or pick Rename from the context menu, to turn the row into an
//!     editable [`TextInput`]; Enter / focus-loss commits.
//!   * **Right-click** a role → a self-routing [`ContextMenu`] with
//!     Delete / Rename / Duplicate.
//!   * **Drag-to-reorder** roles: the drop position rewrites the
//!     precedence so the visible top-to-bottom order *is* the hierarchy
//!     (top = strongest). Wired straight to
//!     [`EditorState::set_role_order`].

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Line, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{TextInput, Widget};
use ui::ColorCompat;

use super::draw::{packed_color, Sizes};
use super::shared::{EditorTab, Shared};
use crate::model::{MemberId, RoleId};

/// Index of each context-menu action, so `take_selection` is unambiguous.
const MENU_RENAME: usize = 0;
const MENU_DUPLICATE: usize = 1;
// index 2 is a separator
const MENU_DELETE: usize = 3;

/// Vertical rhythm — generous rows so the larger type has room (contacts
/// uses 62; a denser list pane sits comfortably at 52).
const ROW_H: f64 = 52.0;
const PAD: f64 = 16.0;
const HEADER_H: f64 = 56.0;
/// How far the pointer must move after press before a click becomes a drag.
const DRAG_THRESHOLD: f64 = 6.0;

/// In-flight press on a role row. Every left-press on a role arms one of
/// these; what it *becomes* is decided on release:
///   * crossed the move threshold → a drag-reorder;
///   * a second click on the already-selected role's name → inline rename;
///   * otherwise → a plain select.
struct Drag {
    role: RoleId,
    /// Pointer y at press, to apply the threshold.
    press_y: f64,
    /// Current pointer y while dragging.
    cur_y: f64,
    /// Whether the threshold has been crossed (a real drag, not a click).
    active: bool,
    /// The press landed on the *already-selected* role's name region, so a
    /// click-without-drag should open the inline rename.
    rename_intent: bool,
}

pub struct SidebarPane {
    shared: Shared,

    rect: Rect,
    /// Row hit rects, parallel to the current visible order.
    rows: Vec<Rect>,
    list_scroll: f64,
    last_mouse: Point,

    /// Inline rename: which role is being renamed + the edit field.
    renaming: Option<RoleId>,
    rename_input: TextInput,
    rename_rect: Rect,

    /// Right-click menu + the role it targets.
    menu: ContextMenu,
    menu_target: Option<RoleId>,

    drag: Option<Drag>,

    dirty: bool,
    relayout: bool,
}

impl SidebarPane {
    pub fn new(shared: Shared) -> Self {
        SidebarPane {
            shared,
            rect: Rect::ZERO,
            rows: Vec::new(),
            list_scroll: 0.0,
            last_mouse: Point::ZERO,
            renaming: None,
            rename_input: TextInput::local().with_id("permissions_role_rename"),
            rename_rect: Rect::ZERO,
            menu: ContextMenu::new(),
            menu_target: None,
            drag: None,
            dirty: true,
            relayout: true,
        }
    }

    fn tab(&self) -> EditorTab {
        self.shared.borrow().tab
    }

    /// The visible role / member ids in their drawn (top-to-bottom) order.
    fn visible_ids(&self) -> Vec<RowId> {
        let s = self.shared.borrow();
        match s.tab {
            EditorTab::Roles => s.visible_roles().into_iter().map(RowId::Role).collect(),
            EditorTab::Members => s.visible_members().into_iter().map(RowId::Member).collect(),
        }
    }

    fn layout(&mut self) {
        self.rows.clear();
        let body_top = self.rect.y0 + HEADER_H + 8.0;
        let count = self.visible_ids().len();
        let max_scroll = (count as f64 * ROW_H - (self.rect.height() - HEADER_H - 8.0)).max(0.0);
        self.list_scroll = self.list_scroll.clamp(0.0, max_scroll);
        let mut y = body_top - self.list_scroll;
        for _ in 0..count {
            self.rows.push(Rect::new(self.rect.x0 + 8.0, y, self.rect.x1 - 8.0, y + ROW_H - 6.0));
            y += ROW_H;
        }
        // Inline rename field tracks the selected role's row.
        if let Some(rid) = self.renaming {
            if let Some(rr) = self.row_rect_of(RowId::Role(rid)) {
                self.rename_rect = Rect::new(rr.x0 + 28.0, rr.y0 + 6.0, rr.x1 - 8.0, rr.y0 + 6.0 + 28.0);
                self.rename_input.apply_layout_external(self.rename_rect);
            }
        }
    }

    fn row_rect_of(&self, id: RowId) -> Option<Rect> {
        self.visible_ids().iter().zip(self.rows.iter()).find(|(v, _)| **v == id).map(|(_, r)| *r)
    }

    fn begin_rename(&mut self, rid: RoleId) {
        let name = self.shared.borrow().set.role(rid).map(|r| r.name.clone()).unwrap_or_default();
        self.rename_input.set_text(&name);
        self.rename_input.set_cursor_pos(name.chars().count());
        self.rename_input.set_focus(true);
        self.renaming = Some(rid);
        self.relayout = true;
        self.dirty = true;
    }

    fn commit_rename(&mut self) {
        if let Some(rid) = self.renaming.take() {
            let new = self.rename_input.value().trim().to_string();
            if !new.is_empty() {
                self.shared.borrow_mut().rename_role(rid, &new);
            }
            self.rename_input.set_focus(false);
            self.relayout = true;
            self.dirty = true;
        }
    }

    fn open_menu_for(&mut self, rid: RoleId, at: Point) {
        self.menu = ContextMenu::new();
        self.menu.push(MenuItem::action("Rename", || {}));
        self.menu.push(MenuItem::action("Duplicate", || {}));
        self.menu.push(MenuItem::separator());
        self.menu.push(MenuItem::action("Delete", || {}));
        self.menu.open_at([at.x as f32, at.y as f32]);
        self.menu_target = Some(rid);
        self.dirty = true;
    }

    /// Apply a context-menu choice to the targeted role.
    fn apply_menu(&mut self, idx: usize) {
        let Some(rid) = self.menu_target else { return };
        match idx {
            MENU_RENAME => self.begin_rename(rid),
            MENU_DUPLICATE => {
                self.shared.borrow_mut().duplicate_role(rid);
                self.relayout = true;
                self.dirty = true;
            }
            MENU_DELETE => {
                if self.renaming == Some(rid) {
                    self.renaming = None;
                }
                self.shared.borrow_mut().delete_role(rid);
                self.relayout = true;
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Compute the order resulting from dropping the dragged role at the
    /// current pointer y, and commit it (rewrites precedence).
    fn finish_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        if !drag.active {
            return;
        }
        if self.tab() != EditorTab::Roles {
            return;
        }
        let order: Vec<RoleId> = {
            let s = self.shared.borrow();
            s.visible_roles()
        };
        // The drop index is determined by how many row-centers sit above
        // the pointer. Compare against the *static* (pre-drag) row layout.
        let body_top = self.rect.y0 + HEADER_H + 8.0 - self.list_scroll;
        let mut drop_idx = 0usize;
        for i in 0..order.len() {
            let center = body_top + i as f64 * ROW_H + (ROW_H - 6.0) / 2.0;
            if drag.cur_y > center {
                drop_idx = i + 1;
            }
        }
        let from = order.iter().position(|r| *r == drag.role);
        let Some(from) = from else { return };
        let mut new_order = order.clone();
        new_order.remove(from);
        // Account for the removed element shifting indices.
        let insert_at = if drop_idx > from { drop_idx - 1 } else { drop_idx };
        let insert_at = insert_at.min(new_order.len());
        new_order.insert(insert_at, drag.role);
        if new_order != order {
            self.shared.borrow_mut().set_role_order(&new_order);
            self.relayout = true;
        }
        self.dirty = true;
    }

    // ---- drawing ----------------------------------------------------

    fn draw_pane(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);
        // Right border separating the sidebar from the content.
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((self.rect.x1, self.rect.y0), (self.rect.x1, self.rect.y1)));

        // Header: tab title + count.
        let tab = self.tab();
        let (title, count) = {
            let s = self.shared.borrow();
            match tab {
                EditorTab::Roles => ("Roles", s.set.roles.len()),
                EditorTab::Members => ("Members", s.set.members.len()),
            }
        };
        tr.draw_text_weighted(scene, title, sz.title, c.text_primary, t * Affine::translate((self.rect.x0 + PAD, self.rect.y0 + 16.0)), None, TextWeight::Bold);
        let sub = format!("{count}");
        tr.draw_text(scene, &sub, sz.secondary, c.text_dim, t * Affine::translate((self.rect.x0 + PAD, self.rect.y0 + 16.0 + sz.title as f64 + 2.0)), None);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((self.rect.x0, self.rect.y0 + HEADER_H), (self.rect.x1, self.rect.y0 + HEADER_H)));

        // Rows.
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &Rect::new(self.rect.x0, self.rect.y0 + HEADER_H, self.rect.x1, self.rect.y1));
        let ids = self.visible_ids();
        let dragging = self.drag.as_ref().filter(|d| d.active).map(|d| d.role);
        for (id, rr) in ids.iter().zip(self.rows.clone().iter()) {
            if rr.y1 < self.rect.y0 + HEADER_H || rr.y0 > self.rect.y1 {
                continue;
            }
            // The actively-dragged row is drawn floating under the cursor;
            // skip it here.
            if let (RowId::Role(rid), Some(drag_rid)) = (*id, dragging) {
                if rid == drag_rid {
                    continue;
                }
            }
            self.draw_row(scene, t, tr, theme, *id, *rr);
        }
        // Floating dragged row.
        if let Some(drag) = self.drag.as_ref().filter(|d| d.active) {
            let h = ROW_H - 6.0;
            let rr = Rect::new(self.rect.x0 + 8.0, drag.cur_y - h / 2.0, self.rect.x1 - 8.0, drag.cur_y + h / 2.0);
            self.draw_row(scene, t, tr, theme, RowId::Role(drag.role), rr);
        }
        scene.pop_layer();

        // Inline rename field on top of its row.
        if self.renaming.is_some() {
            self.rename_input.draw(scene, t, tr, theme, presence);
        }

        // The context menu draws itself last (overlay).
        self.menu.draw(scene, t, tr, theme, presence);
    }

    fn draw_row(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, id: RowId, rr: Rect) {
        let c = &theme.tokens.colors;
        let sz = Sizes::from_theme(theme);
        let s = self.shared.borrow();
        let selected = match id {
            RowId::Role(rid) => s.selected_role == Some(rid),
            RowId::Member(mid) => s.selected_member == Some(mid),
        };
        if selected {
            let bg = RoundedRect::from_rect(rr, theme.tokens.radii.md as f64);
            scene.fill(Fill::NonZero, t, theme.accent_primary.with_alpha_factor(0.16), None, &bg);
        }
        let (name, sub, color) = match id {
            RowId::Role(rid) => {
                let Some(role) = s.set.role(rid) else { return };
                (role.name.clone(), format!("{} grants", role.grants.len()), Some(role.color))
            }
            RowId::Member(mid) => {
                let Some(m) = s.set.member(mid) else { return };
                let n = m.role_ids.len();
                (m.display_name.clone(), format!("{} role{}", n, if n == 1 { "" } else { "s" }), None)
            }
        };
        let tx = if let Some(rgb) = color {
            let dot = Circle::new(Point::new(rr.x0 + 16.0, rr.center().y), 7.0);
            scene.fill(Fill::NonZero, t, packed_color(rgb), None, &dot);
            rr.x0 + 30.0
        } else {
            rr.x0 + 14.0
        };
        // Hide the static name while inline-renaming this row.
        let renaming_this = matches!(id, RowId::Role(rid) if self.renaming == Some(rid));
        if !renaming_this {
            tr.draw_text_weighted(scene, &name, sz.body + 1.0, c.text_primary, t * Affine::translate((tx, rr.y0 + 8.0)), Some((rr.x1 - tx - 8.0) as f32), TextWeight::Medium);
        }
        tr.draw_text(scene, &sub, sz.secondary, c.text_dim, t * Affine::translate((tx, rr.y0 + 8.0 + sz.body as f64 + 6.0)), None);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowId {
    Role(RoleId),
    Member(MemberId),
}

impl Widget for SidebarPane {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        self.rename_input.sync(state, path);
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
            if let Some(drag) = self.drag.as_mut() {
                drag.cur_y = self.last_mouse.y;
                if (drag.cur_y - drag.press_y).abs() > DRAG_THRESHOLD {
                    drag.active = true;
                }
                self.dirty = true;
            }
        }

        // The context menu self-routes first when open.
        if self.menu.is_open() {
            self.menu.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
            if let Some(idx) = self.menu.take_selection() {
                self.apply_menu(idx);
                *handled = true;
            }
            if *handled {
                return;
            }
        }

        // Inline rename field gets keyboard + clicks while active.
        if self.renaming.is_some() {
            if let WindowEvent::KeyboardInput { event: ke, .. } = event {
                if ke.state == ElementState::Pressed {
                    match &ke.logical_key {
                        Key::Named(NamedKey::Enter) => {
                            self.commit_rename();
                            *handled = true;
                            return;
                        }
                        Key::Named(NamedKey::Escape) => {
                            self.renaming = None;
                            self.rename_input.set_focus(false);
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                        _ => {}
                    }
                }
            }
            // A click outside the rename field commits it; a click inside
            // is routed to the field for caret placement.
            if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
                if !self.rename_rect.contains(self.last_mouse) {
                    self.commit_rename();
                    // fall through to normal click handling below
                } else {
                    self.rename_input.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
                    *handled = true;
                    return;
                }
            } else {
                let _ = self.rename_input.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
                if *handled {
                    return;
                }
            }
        }

        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Right, .. } => {
                // Right-click a role → context menu.
                if self.tab() == EditorTab::Roles {
                    let ids = self.visible_ids();
                    for (id, rr) in ids.iter().zip(self.rows.clone().iter()) {
                        if rr.contains(self.last_mouse) {
                            if let RowId::Role(rid) = *id {
                                // Select then open the menu anchored at the cursor.
                                self.shared.borrow_mut().selected_role = Some(rid);
                                self.open_menu_for(rid, self.last_mouse);
                                *handled = true;
                                return;
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                let p = self.last_mouse;
                let ids = self.visible_ids();
                for (id, rr) in ids.iter().zip(self.rows.clone().iter()) {
                    if rr.contains(p) {
                        match *id {
                            RowId::Role(rid) => {
                                let already = self.shared.borrow().selected_role == Some(rid);
                                self.shared.borrow_mut().selected_role = Some(rid);
                                // Every press on a role arms a potential
                                // drag. If this press was a *second* click on
                                // the already-selected role's name and no
                                // drag follows, release turns it into an
                                // inline rename instead.
                                let name_x = rr.x0 + 30.0;
                                let rename_intent =
                                    already && p.x > name_x && self.renaming.is_none();
                                self.drag = Some(Drag { role: rid, press_y: p.y, cur_y: p.y, active: false, rename_intent });
                            }
                            RowId::Member(mid) => {
                                self.shared.borrow_mut().selected_member = Some(mid);
                            }
                        }
                        self.dirty = true;
                        self.relayout = true;
                        *handled = true;
                        return;
                    }
                }
            }
            WindowEvent::MouseInput { state: ElementState::Released, button: MouseButton::Left, .. } => {
                if let Some(drag) = self.drag.take() {
                    if drag.active {
                        // A real drag → reorder.
                        self.drag = Some(drag);
                        self.finish_drag();
                    } else if drag.rename_intent {
                        // Click-in-place on the selected name → inline rename.
                        self.begin_rename(drag.role);
                    }
                    self.dirty = true;
                    *handled = true;
                    return;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => (-y * 48.0) as f64,
                    winit::event::MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                self.list_scroll += dy;
                self.relayout = true;
                self.dirty = true;
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "PermissionsSidebar"
    }
    fn test_id(&self) -> Option<&str> {
        Some("PermissionsSidebar")
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
        self.rename_input.needs_commit()
    }
    fn on_commit(&mut self) {
        self.rename_input.on_commit();
    }
}

// =====================================================================
// Tests — drive the REAL handle_event path at computed rects.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::shared::EditorState;
    use crate::sample;
    use automerge::ROOT;
    use taffy::AvailableSpace;
    use winit::event::KeyEvent;

    fn shared() -> Shared {
        let s = EditorState::new(sample::sample_registry(), sample::sample_set());
        s.borrow_mut().ensure_selection();
        s
    }

    fn laid_out(pane: &mut SidebarPane, w: f64, h: f64) -> (TaffyTree<()>, NodeId) {
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = pane.build_layout(&mut taffy, &theme, &mut node_map);
        let root = taffy
            .new_with_children(Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() }, &[child])
            .unwrap();
        taffy
            .compute_layout(root, Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) })
            .unwrap();
        pane.apply_layout(&taffy, child, Point::ORIGIN);
        (taffy, child)
    }

    fn cursor(pane: &mut SidebarPane, taffy: &TaffyTree<()>, node: NodeId, p: Point) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        let ev = WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(p.x, p.y),
        };
        pane.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
    }

    fn press(pane: &mut SidebarPane, taffy: &TaffyTree<()>, node: NodeId, button: MouseButton) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        let ev = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button };
        pane.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
    }

    fn release(pane: &mut SidebarPane, taffy: &TaffyTree<()>, node: NodeId) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        let ev = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Released, button: MouseButton::Left };
        pane.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
    }

    /// Set the rename field's text, then drive a real Enter key through
    /// `handle_event` to commit (the same path the live app takes). The
    /// zeroed-KeyEvent trick mirrors the toolkit's own widget tests
    /// (winit makes `platform_specific` private, so a literal won't build).
    fn type_text(pane: &mut SidebarPane, taffy: &TaffyTree<()>, node: NodeId, s: &str) {
        pane.rename_input.set_text(s);
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Named(NamedKey::Enter);
        ev.physical_key = winit::keyboard::PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
        ev.location = winit::keyboard::KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = None;
        let enter = WindowEvent::KeyboardInput { device_id: unsafe { std::mem::zeroed() }, event: ev, is_synthetic: false };
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        pane.handle_event(&enter, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, node, taffy);
    }

    fn nth_role_row_center(pane: &SidebarPane, n: usize) -> Point {
        pane.rows[n].center()
    }

    #[test]
    fn click_then_click_name_renames_inline() {
        let mut pane = SidebarPane::new(shared());
        let (taffy, node) = laid_out(&mut pane, 300.0, 800.0);
        // Top role's row; full click to select, second full click on its
        // name to begin the inline rename (drag vs rename is decided on
        // release — a press+release in place with no movement renames).
        let p = nth_role_row_center(&pane, 0);
        let name_pt = Point::new(p.x + 30.0, p.y); // past the dot+x offset
        cursor(&mut pane, &taffy, node, name_pt);
        press(&mut pane, &taffy, node, MouseButton::Left); // select (press)
        release(&mut pane, &taffy, node); // release in place
        cursor(&mut pane, &taffy, node, name_pt);
        press(&mut pane, &taffy, node, MouseButton::Left); // rename intent (press)
        release(&mut pane, &taffy, node); // release in place → begin rename
        assert!(pane.renaming.is_some(), "second click on selected name starts inline rename");
        // Re-layout so the rename field rect is current.
        laid_out(&mut pane, 300.0, 800.0);
        let rid = pane.renaming.unwrap();
        type_text(&mut pane, &taffy, node, "Captain");
        assert!(pane.renaming.is_none(), "Enter commits + closes the rename");
        assert_eq!(pane.shared.borrow().set.role(rid).unwrap().name, "Captain");
    }

    #[test]
    fn right_click_menu_delete_removes_role() {
        let mut pane = SidebarPane::new(shared());
        let (taffy, node) = laid_out(&mut pane, 300.0, 800.0);
        let before = pane.shared.borrow().set.roles.len();
        let target = pane.visible_ids()[1];
        let RowId::Role(rid) = target else { panic!() };
        let p = pane.rows[1].center();
        cursor(&mut pane, &taffy, node, p);
        press(&mut pane, &taffy, node, MouseButton::Right);
        assert!(pane.menu.is_open(), "right-click opens the context menu");
        // Click the Delete row in the menu.
        pane.menu.click_item(MENU_DELETE);
        if let Some(idx) = pane.menu.take_selection() {
            pane.apply_menu(idx);
        }
        assert_eq!(pane.shared.borrow().set.roles.len(), before - 1);
        assert!(pane.shared.borrow().set.role(rid).is_none(), "deleted role is gone");
    }

    #[test]
    fn right_click_menu_duplicate_adds_a_copy() {
        let mut pane = SidebarPane::new(shared());
        let (taffy, node) = laid_out(&mut pane, 300.0, 800.0);
        let before = pane.shared.borrow().set.roles.len();
        let RowId::Role(rid) = pane.visible_ids()[0] else { panic!() };
        let src_name = pane.shared.borrow().set.role(rid).unwrap().name.clone();
        let row0 = pane.rows[0].center();
        cursor(&mut pane, &taffy, node, row0);
        press(&mut pane, &taffy, node, MouseButton::Right);
        pane.menu.click_item(MENU_DUPLICATE);
        if let Some(idx) = pane.menu.take_selection() {
            pane.apply_menu(idx);
        }
        assert_eq!(pane.shared.borrow().set.roles.len(), before + 1);
        let copy = format!("{src_name} (copy)");
        assert!(pane.shared.borrow().set.roles.iter().any(|r| r.name == copy), "a (copy) role was added");
    }

    #[test]
    fn drag_reorder_changes_order_and_resolution() {
        use crate::check::check;
        use crate::model::Grant;
        // Build a set where two roles conflict on one permission so the
        // resolution flips when their order flips.
        let s = EditorState::new(sample::sample_registry(), {
            let mut set = crate::model::PermissionSet::default();
            // Top role denies; bottom role allows. Resolution = top's = Deny.
            let mut top = crate::model::Role::new("Muted", 0xff0000, 1);
            top.set_grant("chat.send_message", None, Grant::Deny);
            let mut bottom = crate::model::Role::new("Member", 0x00ff00, 0);
            bottom.set_grant("chat.send_message", None, Grant::Allow);
            let (tid, bid) = (top.id, bottom.id);
            set.roles.push(top);
            set.roles.push(bottom);
            let mut m = crate::model::Member::new("Ada");
            m.role_ids.push(tid);
            m.role_ids.push(bid);
            set.members.push(m);
            set
        });
        s.borrow_mut().ensure_selection();
        let mut pane = SidebarPane::new(s.clone());
        let (taffy, node) = laid_out(&mut pane, 300.0, 800.0);

        // Initially "Muted" is strongest (top) → send is Deny.
        let mid = s.borrow().set.members[0].id;
        let decision_before = {
            let st = s.borrow();
            let m = st.set.member(mid).unwrap().clone();
            check(&st.registry, &st.set, &m, "chat.send_message", None)
        };
        assert_eq!(decision_before, crate::model::Decision::Deny, "top (Muted) denies before reorder");

        // Drag the TOP role (Muted) down below the bottom one.
        let top_center = pane.rows[0].center();
        let bottom_center = pane.rows[1].center();
        cursor(&mut pane, &taffy, node, top_center);
        press(&mut pane, &taffy, node, MouseButton::Left); // arm drag on top role
        // Move past the bottom row's center to drop below it.
        cursor(&mut pane, &taffy, node, Point::new(bottom_center.x, bottom_center.y + 30.0));
        release(&mut pane, &taffy, node);

        // Now "Member" (Allow) is strongest → send flips to Allow.
        let decision_after = {
            let st = s.borrow();
            let m = st.set.member(mid).unwrap().clone();
            check(&st.registry, &st.set, &m, "chat.send_message", None)
        };
        assert_eq!(decision_after, crate::model::Decision::Allow, "after dragging Muted to the bottom, Member's Allow wins");
    }
}
