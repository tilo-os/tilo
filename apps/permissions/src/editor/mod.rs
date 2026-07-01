//! The reusable permission editor.
//!
//! The editor is two `Widget`s — a [`sidebar::SidebarPane`] (the
//! roles/members list) and a [`detail::DetailPane`] (the grid / member
//! detail) — composed by the toolkit's responsive
//! [`ui::widget::NavSplit`]. The split docks the sidebar when the window
//! is wide and floats it as an overlay flap (behind a scrim) when narrow,
//! so the roles/members list is a real, adaptive sidebar rather than a
//! fixed column.
//!
//! [`EditorView`] is the public root: it owns the shared [`EditorState`]
//! (mirrored to the CRDT), the `NavSplit`, and a thin top tab bar that
//! switches between the Roles and Members screens. The standalone
//! `permissions` binary wraps it; any host embeds the same view via
//! [`crate::seam::open_editor`].

mod detail;
mod draw;
mod shared;
mod sidebar;

use std::collections::HashMap;

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Line, Point, Rect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{NavSplit, SplitMode, Widget};

use crate::doc::PermissionsDoc;
use crate::model::PermissionSet;
use crate::registry::PermissionRegistry;
use crate::sample;
use crate::seam::EditorIntent;

pub use shared::EditorTab;

use self::detail::DetailPane;
use self::draw::{seg_button, Sizes};
use self::shared::{EditorState, Shared};
use self::sidebar::SidebarPane;

const TABBAR_H: f64 = 52.0;
const PAD: f64 = 16.0;
/// Width below which the sidebar collapses to an overlay flap. Roughly a
/// phone/half-tile width; above it the sidebar docks.
const BREAKPOINT: f64 = 760.0;
const SIDEBAR_W: f64 = 280.0;

/// The editor widget — the public root. Owns the shared state, the
/// responsive split, and the top tab bar.
pub struct EditorView {
    shared: Shared,
    split: NavSplit,

    rect: Rect,
    tab_roles: Rect,
    tab_members: Rect,
    new_item: Rect,
    /// Hamburger toggle for the overlay sidebar. Non-empty only in the
    /// narrow (overlay) layout, where the roles/members list is a closed
    /// flap and needs an explicit affordance to open it.
    menu_btn: Rect,
    /// Whether the current width resolves to the narrow overlay layout —
    /// re-derived from the allocated width every layout pass so a resize
    /// between breakpoints reflows the tab bar (and shows/hides the toggle).
    overlay: bool,

    last_mouse: Point,
    seeded: bool,
    dirty: bool,
    relayout: bool,
    /// `--shots`-only: force the overlay flap open so a single static frame
    /// can capture the revealed narrow sidebar. Never set by the live loop.
    force_open_sidebar: bool,
}

impl Default for EditorView {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorView {
    pub fn new() -> Self {
        Self::build(sample::sample_registry(), PermissionSet::default(), EditorTab::Roles)
    }

    fn build(registry: PermissionRegistry, set: PermissionSet, tab: EditorTab) -> Self {
        let shared = EditorState::new(registry, set);
        shared.borrow_mut().tab = tab;
        let split = NavSplit::new(
            Box::new(SidebarPane::new(shared.clone())),
            Box::new(DetailPane::new(shared.clone())),
        )
        .with_mode(SplitMode::Adaptive)
        .with_breakpoint(BREAKPOINT)
        .with_sidebar_width(SIDEBAR_W);
        EditorView {
            shared,
            split,
            rect: Rect::ZERO,
            tab_roles: Rect::ZERO,
            tab_members: Rect::ZERO,
            new_item: Rect::ZERO,
            menu_btn: Rect::ZERO,
            overlay: false,
            last_mouse: Point::ZERO,
            seeded: false,
            dirty: true,
            relayout: true,
            force_open_sidebar: false,
        }
    }

    /// `--shots`-only: open the overlay flap for the captured frame so the
    /// narrow revealed sidebar is visible. Has no effect on the live app.
    pub fn shots_open_sidebar(mut self) -> Self {
        self.force_open_sidebar = true;
        self
    }

    /// Build an editor pre-seeded with the sample set + identity model and a
    /// chosen tab + selected role/member index, ready to render a single
    /// deterministic `--shots` frame (the live app seeds lazily on first
    /// `sync`; for a screenshot we need the state populated up front so a
    /// specific role/member can be selected). `nth_role` selects the nth
    /// *visible* (strongest-first) role; `nth_member` the nth visible member.
    pub fn seeded_sample(tab: EditorTab, nth_role: usize, nth_member: usize) -> Self {
        let mut v = Self::build(sample::sample_registry(), sample::sample_set(), tab);
        {
            let mut s = v.shared.borrow_mut();
            s.identities = sample::sample_identities();
            let roles = s.visible_roles();
            if let Some(rid) = roles.get(nth_role).copied() {
                s.selected_role = Some(rid);
            }
            let members = s.visible_members();
            if let Some(mid) = members.get(nth_member).copied() {
                s.selected_member = Some(mid);
            }
            s.ensure_selection();
        }
        // Already populated — don't let the first `sync` overwrite it.
        v.seeded = true;
        v
    }

    /// Construct from a [`EditorIntent`] — the shared entry point used by
    /// the standalone app and every in-context host.
    pub fn from_intent(intent: EditorIntent) -> Self {
        let v = Self::new();
        {
            let mut s = v.shared.borrow_mut();
            match intent {
                EditorIntent::Roles { select } => {
                    s.tab = EditorTab::Roles;
                    s.selected_role = select;
                }
                EditorIntent::Members { select } => {
                    s.tab = EditorTab::Members;
                    s.selected_member = select;
                }
                EditorIntent::Context { context, tab } => {
                    s.tab = tab;
                    s.active_context = Some(context);
                }
            }
        }
        v
    }

    /// Override the registry (a host injects the live, app-contributed
    /// registry instead of the sample one).
    pub fn with_registry(self, registry: PermissionRegistry) -> Self {
        self.shared.borrow_mut().registry = registry;
        self
    }

    /// Inject the live identity roster — the production seam to the
    /// collaboration/identity layer. A host builds this from the
    /// [`ui::collaboration::CollabEvent::Context`] roster via
    /// [`crate::identity::IdentitySource::from_collaborators`]; until that is
    /// available the editor uses the sample identity model. Members resolve
    /// their display name + colour through this on the next frame.
    pub fn with_identities(self, identities: crate::identity::IdentitySource) -> Self {
        self.shared.borrow_mut().set_identities(identities);
        self
    }

    /// Push an updated live roster mid-session (e.g. a peer joined/left, or
    /// `CollabEvent::Context` re-delivered the collaborators). Marks the
    /// member views dirty so names/colours re-resolve.
    pub fn update_identities(&mut self, identities: crate::identity::IdentitySource) {
        self.shared.borrow_mut().set_identities(identities);
        self.dirty = true;
        self.relayout = true;
    }

    // Read-only access for tests/hosts.
    pub fn set(&self) -> std::cell::Ref<'_, PermissionSet> {
        std::cell::Ref::map(self.shared.borrow(), |s| &s.set)
    }
    pub fn active_tab(&self) -> EditorTab {
        self.shared.borrow().tab
    }
    pub fn registry_len(&self) -> usize {
        self.shared.borrow().registry.len()
    }

    fn layout(&mut self) {
        let r = self.rect;
        let tab_y = r.y0 + 10.0;
        // Re-derive the responsive mode from the CURRENT width every pass so
        // a resize across the breakpoint reflows: the split occupies the full
        // width below the tab bar, so its docked/overlay choice keys on the
        // same width the toggle does.
        self.overlay = r.width() < BREAKPOINT;
        let mut x = r.x0 + PAD;
        if self.overlay {
            self.menu_btn = Rect::new(x, tab_y, x + 40.0, tab_y + 32.0);
            x = self.menu_btn.x1 + 10.0;
        } else {
            self.menu_btn = Rect::ZERO;
        }
        self.tab_roles = Rect::new(x, tab_y, x + 96.0, tab_y + 32.0);
        self.tab_members = Rect::new(self.tab_roles.x1 + 8.0, tab_y, self.tab_roles.x1 + 8.0 + 112.0, tab_y + 32.0);
        // "+ New Member" is wider than "+ New Role"; size the button to the
        // active label so it never clips at the narrow width.
        let new_w = if self.shared.borrow().tab == EditorTab::Members { 148.0 } else { 120.0 };
        self.new_item = Rect::new(r.x1 - PAD - new_w, tab_y, r.x1 - PAD, tab_y + 32.0);
    }

    fn draw_tabbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TABBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((bar.x0, bar.y1), (bar.x1, bar.y1)));

        // Overlay layout: a hamburger reveals the roles/members flap (it is
        // closed by default when narrow). Highlighted while the flap is open.
        if self.overlay {
            draw::menu_button(scene, t, theme, self.menu_btn, self.split.is_sidebar_open());
        }

        let tab = self.shared.borrow().tab;
        seg_button(scene, t, tr, theme, self.tab_roles, "Roles", tab == EditorTab::Roles);
        seg_button(scene, t, tr, theme, self.tab_members, "Members", tab == EditorTab::Members);

        let new_label = match tab {
            EditorTab::Roles => "+ New Role",
            EditorTab::Members => "+ New Member",
        };
        draw::toolbar_button(scene, t, tr, theme, self.new_item, new_label, true);
        let _ = Sizes::from_theme(theme);
    }

    fn render(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, perms: &PermissionContext) {
        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);
        self.draw_tabbar(scene, t, tr, theme);
        self.split.draw_with_presence(scene, t, tr, theme, presence, perms);
    }

    fn flush(&mut self, doc: &mut AutoCommit) {
        let mut s = self.shared.borrow_mut();
        if s.pending_write {
            PermissionsDoc::write(doc, &s.set);
            s.pending_write = false;
        }
    }
}

impl Widget for EditorView {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId) {
        if !self.seeded {
            // Load whatever the synced/persisted document holds. A fresh
            // workspace's document is empty, so the editor opens empty (with
            // its "no roles yet" guidance) rather than inventing roles — the
            // authored set only ever comes from real user edits or a peer's
            // synced state. `--shots`/tests take the pre-seeded path instead.
            let existing = PermissionsDoc::read(state);
            {
                let mut s = self.shared.borrow_mut();
                s.set = existing;
                s.ensure_selection();
            }
            self.seeded = true;
            self.relayout = true;
            self.dirty = true;
        }
        self.flush(state);
        self.split.sync(state, path);
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

        // The split occupies everything below the tab bar. The live App loop
        // drives no reveal animation for a split rebuilt each frame, so the
        // flap is snapped to its resolved open state. Snapping AFTER the
        // children are placed would leave them one frame behind the reveal —
        // and since a toggle triggers only a single relayout, a freshly
        // opened overlay would get stuck off-screen. So lay the split out,
        // read the resolved state, snap, and (only when the snap actually
        // moved the reveal) lay it out once more so the sidebar's rects match
        // the reveal on this same frame. Steady-state frames settle in one pass.
        let split_rect = Rect::new(self.rect.x0, self.rect.y0 + TABBAR_H, self.rect.x1, self.rect.y1);
        if self.force_open_sidebar {
            self.split.open_sidebar();
        }
        for _ in 0..2 {
            let mut inner: TaffyTree<()> = TaffyTree::new();
            let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let snode = self.split.build_layout(&mut inner, &Theme::dark(), &mut map);
            let _ = inner.compute_layout(
                snode,
                Size {
                    width: taffy::AvailableSpace::Definite(split_rect.width() as f32),
                    height: taffy::AvailableSpace::Definite(split_rect.height() as f32),
                },
            );
            self.split.apply_layout(&inner, snode, Point::new(split_rect.x0, split_rect.y0));
            let desired = self.split.is_sidebar_open();
            let target = if desired { 1.0 } else { 0.0 };
            let settled = (self.split.flap().reveal() - target).abs() < 1e-6;
            self.split.snap_open(desired);
            if settled {
                break;
            }
        }

        self.relayout = false;
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx) {
        self.render(scene, transform, tr, theme, presence, &PermissionContext::permissive());
    }

    fn draw_with_presence(&mut self, scene: &mut Scene, transform: Affine, tr: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx, permissions: &PermissionContext) {
        self.render(scene, transform, tr, theme, presence, permissions);
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

        // Tab bar clicks first.
        if let WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } = event {
            let p = self.last_mouse;
            if self.overlay && self.menu_btn.contains(p) {
                self.split.toggle_sidebar();
                self.dirty = true;
                self.relayout = true;
                *handled = true;
                return;
            }
            if self.tab_roles.contains(p) {
                self.shared.borrow_mut().select_tab(EditorTab::Roles);
                self.dirty = true;
                self.relayout = true;
                *handled = true;
                return;
            }
            if self.tab_members.contains(p) {
                self.shared.borrow_mut().select_tab(EditorTab::Members);
                self.dirty = true;
                self.relayout = true;
                *handled = true;
                return;
            }
            if self.new_item.contains(p) {
                let tab = self.shared.borrow().tab;
                match tab {
                    EditorTab::Roles => { self.shared.borrow_mut().create_role(); }
                    EditorTab::Members => { self.shared.borrow_mut().create_member(); }
                }
                self.flush(state);
                self.dirty = true;
                self.relayout = true;
                *handled = true;
                return;
            }
        }

        // Everything below the tab bar → the split (and its two panes).
        // Capture the selection so we can auto-dismiss the overlay flap once
        // the user picks a row from it (Adwaita master-detail behaviour: the
        // list slides away to reveal the chosen role/member).
        let sel_before = {
            let s = self.shared.borrow();
            (s.selected_role, s.selected_member)
        };
        self.split.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);
        if self.overlay && self.split.is_sidebar_open() {
            let sel_after = {
                let s = self.shared.borrow();
                (s.selected_role, s.selected_member)
            };
            if sel_after != sel_before {
                self.split.close_sidebar();
                self.relayout = true;
                self.dirty = true;
            }
        }

        // Any pane mutation marks the shared state dirty / pending; flush
        // and propagate the redraw/relayout signal up. We read BOTH the
        // shared dirty flag (set by `bump()` on every selection + mutation —
        // the primary signal) AND the panes' own layout-dirty/redraw flags
        // via the split's child recursion (belt-and-suspenders, so a pane
        // that only sets its local flags — e.g. an inline rename field
        // resizing — still propagates up). The earlier code read ONLY
        // `s.dirty`, which a bare selection assignment never set — that was
        // the "clicking a role doesn't switch" bug.
        let (d, w) = {
            let s = self.shared.borrow();
            (s.dirty, s.pending_write)
        };
        if d || self.split.is_layout_dirty() {
            self.relayout = true;
        }
        if d || self.split.needs_redraw() {
            self.dirty = true;
        }
        if w {
            self.flush(state);
        }
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
        self.dirty || self.shared.borrow().dirty
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.shared.borrow_mut().dirty = false;
    }
    fn is_layout_dirty(&self) -> bool {
        self.relayout
    }
}

// =====================================================================
// Tests — drive the real top-level handle_event path.
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ROOT;
    use taffy::AvailableSpace;

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
            .new_with_children(Style { size: Size { width: Dimension::length(w as f32), height: Dimension::length(h as f32) }, ..Default::default() }, &[child])
            .unwrap();
        taffy.compute_layout(root, Size { width: AvailableSpace::Definite(w as f32), height: AvailableSpace::Definite(h as f32) }).unwrap();
        view.apply_layout(&taffy, child, Point::ORIGIN);
        let mut scene = Scene::new();
        view.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
        let _ = &scene;
    }

    /// A persistent-doc harness: one `AutoCommit` + one taffy tree reused
    /// across frames + events, exactly like the live `ui::App` loop. Returns
    /// the laid-out taffy/child so events can be dispatched against the real
    /// node. `relayout()` re-runs the layout pass (what the App does when
    /// `is_layout_dirty()` is true after a handled event).
    struct Harness {
        view: EditorView,
        doc: AutoCommit,
        w: f64,
        h: f64,
    }

    impl Harness {
        fn new(w: f64, h: f64) -> Self {
            // The live app starts empty; these interaction tests need a
            // populated editor, so seed the sample set the same way `--shots`
            // does (seeded_sample marks the view so sync won't reset it).
            let mut h_ = Harness { view: EditorView::seeded_sample(EditorTab::Roles, 0, 0), doc: AutoCommit::new(), w, h };
            h_.frame();
            h_
        }

        /// Run sync → build → compute → apply → draw, like one App frame.
        fn frame(&mut self) {
            let theme = Theme::dark();
            let presence = PresenceCtx::empty();
            let permission = PermissionContext::permissive();
            let mut tr = TextRenderer::new();
            let mut taffy: TaffyTree<()> = TaffyTree::new();
            let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            self.view.sync(&mut self.doc, &ROOT);
            let child = self.view.build_layout(&mut taffy, &theme, &mut node_map);
            let root = taffy
                .new_with_children(Style { size: Size { width: Dimension::length(self.w as f32), height: Dimension::length(self.h as f32) }, ..Default::default() }, &[child])
                .unwrap();
            taffy.compute_layout(root, Size { width: AvailableSpace::Definite(self.w as f32), height: AvailableSpace::Definite(self.h as f32) }).unwrap();
            self.view.apply_layout(&taffy, child, Point::ORIGIN);
            let mut scene = Scene::new();
            self.view.draw_with_presence(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence, &permission);
        }

        /// Dispatch a full left-click at `p` through the REAL top-level
        /// `EditorView::handle_event` path (cursor-move then press), then —
        /// if the editor reported it needs a relayout (the App's gate) —
        /// re-run the layout/draw frame so the grid reflects the new state.
        /// Returns whether a relayout was requested (the bug's tell: a
        /// selection click must request one).
        fn click(&mut self, p: Point) -> bool {
            let mut handled = false;
            let mut ft = None;
            let empty = TaffyTree::new();
            let mv = WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(p.x, p.y) };
            self.view.handle_event(&mv, 1.0, &Default::default(), &mut self.doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &empty);
            let ev = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left };
            self.view.handle_event(&ev, 1.0, &Default::default(), &mut self.doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &empty);
            // Release so a role press doesn't leave an armed drag.
            let rel = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Released, button: MouseButton::Left };
            self.view.handle_event(&rel, 1.0, &Default::default(), &mut self.doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &empty);
            let needs_relayout = self.view.is_layout_dirty();
            if needs_relayout {
                self.frame();
            }
            needs_relayout
        }

        /// Center of the docked sidebar's nth role row, from the same layout
        /// constants the sidebar uses (docked sidebar sits flush left below
        /// the tab bar). Wide layout only (docked).
        fn role_row_center(&self, n: usize) -> Point {
            let sidebar_x0 = self.view.rect.x0;
            let sidebar_top = self.view.rect.y0 + TABBAR_H; // split band top
            // sidebar HEADER_H = 56, body pad 8, ROW_H = 52, row inset top 0,
            // row height ROW_H-6 = 46.
            let body_top = sidebar_top + 56.0 + 8.0;
            let y = body_top + n as f64 * 52.0 + (52.0 - 6.0) / 2.0;
            // x at the colour-dot region (x0+16), LEFT of the name region
            // (x0+30) — a press here always plain-selects and never arms the
            // second-click-on-name inline rename, so the test exercises pure
            // selection switching.
            Point::new(sidebar_x0 + 16.0, y)
        }
    }

    /// The real-not-mock contract: a fresh workspace document seeds NOTHING.
    /// The live app must never fabricate roles/members on first sync — the
    /// authored set only comes from real edits or a peer's synced state.
    #[test]
    fn fresh_doc_seeds_no_fabricated_content() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        assert!(v.set().roles.is_empty(), "no roles are fabricated on first launch");
        assert!(v.set().members.is_empty(), "no members are fabricated on first launch");
        assert!(v.shared.borrow().selected_role.is_none(), "nothing to auto-select in an empty set");
    }

    /// The complement: when the document already holds a set (persisted, or
    /// synced from a peer), sync loads that REAL state instead of starting
    /// empty. Proves the seam reads live data, not a hard-coded fixture.
    #[test]
    fn sync_loads_persisted_set_from_the_document() {
        let mut doc = AutoCommit::new();
        PermissionsDoc::write(&mut doc, &sample::sample_set());
        let mut v = EditorView::new();
        v.sync(&mut doc, &ROOT);
        assert!(!v.set().roles.is_empty(), "sync loads the roles already in the document");
        assert!(v.shared.borrow().selected_role.is_some(), "a loaded role auto-selects");
    }

    /// THE BUG: clicking a different role in the sidebar must switch the
    /// selection AND make the editor relayout/redraw so the grid follows.
    /// We click EACH role in turn and assert the selection moved to it and
    /// the click requested a relayout (the missing signal that froze the
    /// grid). Several roles, not one.
    #[test]
    fn clicking_each_role_switches_selection_and_relayouts() {
        let mut h = Harness::new(1280.0, 800.0);
        // The visible (strongest-first) role order the sidebar draws.
        let order = h.view.shared.borrow().visible_roles();
        assert!(order.len() >= 3, "need several roles to prove the switch");

        let mut switched = 0;
        for (n, rid) in order.iter().enumerate() {
            let pre = h.view.shared.borrow().selected_role;
            let relaid = h.click(h.role_row_center(n));
            let now = h.view.shared.borrow().selected_role;
            assert_eq!(now, Some(*rid), "click on row {n} selects that exact role");
            if pre != Some(*rid) {
                assert!(relaid, "selecting a DIFFERENT role (row {n}) must request a relayout — this is the bug");
                switched += 1;
            }
        }
        assert!(switched >= 2, "proved the switch for multiple distinct roles (switched {switched})");

        // And the detail grid now reflects the LAST selected role: the
        // DetailPane's selected role (which the grid draws from) equals it.
        let last = *order.last().unwrap();
        assert_eq!(h.view.shared.borrow().selected_role, Some(last));
    }

    /// Switching back and forth between two roles must keep landing on the
    /// right one every time (no "stuck" selection across repeats).
    #[test]
    fn role_selection_is_not_sticky_across_repeats() {
        let mut h = Harness::new(1280.0, 800.0);
        let order = h.view.shared.borrow().visible_roles();
        let (a, b) = (order[0], order[1]);
        for _ in 0..3 {
            h.click(h.role_row_center(0));
            assert_eq!(h.view.shared.borrow().selected_role, Some(a));
            h.click(h.role_row_center(1));
            assert_eq!(h.view.shared.borrow().selected_role, Some(b));
        }
    }

    #[test]
    fn wide_and_narrow_frames_do_not_panic() {
        let mut v = EditorView::new();
        drive_frame(&mut v, 1280, 800);
        // Narrow → overlay sidebar.
        let mut v2 = EditorView::new();
        drive_frame(&mut v2, 520, 880);
    }

    /// Drive a full left-click at `p` through the REAL top-level event path
    /// (cursor-move, press, release) against a bare EditorView.
    fn editor_click(v: &mut EditorView, p: Point) {
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut ft = None;
        let empty = TaffyTree::new();
        for ev in [
            WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(p.x, p.y) },
            WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left },
            WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Released, button: MouseButton::Left },
        ] {
            v.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &empty);
        }
    }

    /// The responsive breakpoint MUST be re-derived from the CURRENT width
    /// every layout pass. Render ONE instance wide, then narrow, then wide —
    /// the tab-bar toggle and the docked/overlay mode must follow the resize,
    /// never freeze at the first frame's value.
    #[test]
    fn resizing_between_breakpoints_reflows_the_toggle() {
        let mut v = EditorView::seeded_sample(EditorTab::Roles, 0, 0);
        drive_frame(&mut v, 1280, 800);
        assert_eq!(v.split.layout_mode(), ui::widget::SplitLayout::Docked);
        assert!(!v.overlay, "wide layout docks the sidebar");
        assert_eq!(v.menu_btn, Rect::ZERO, "no overlay toggle when docked");

        drive_frame(&mut v, 560, 800);
        assert_eq!(v.split.layout_mode(), ui::widget::SplitLayout::Overlay);
        assert!(v.overlay, "narrow re-derives to overlay from the NEW width, not a cached mode");
        assert!(v.menu_btn.width() > 0.0, "overlay exposes a toggle to open the flap");

        drive_frame(&mut v, 1280, 800);
        assert!(!v.overlay, "resizing back to wide re-docks");
        assert_eq!(v.menu_btn, Rect::ZERO);
    }

    /// On narrow the roles/members list is a closed flap; the hamburger is the
    /// only way to reach it, so it must actually open the flap.
    #[test]
    fn hamburger_opens_the_overlay_flap() {
        let mut v = EditorView::seeded_sample(EditorTab::Roles, 0, 0);
        drive_frame(&mut v, 560, 800);
        assert!(!v.split.is_sidebar_open(), "overlay flap starts closed");
        let btn = v.menu_btn.center();
        editor_click(&mut v, btn);
        assert!(v.split.is_sidebar_open(), "the hamburger opened the roles/members flap");
    }

    /// The full narrow master-detail flow: open the flap, pick a different
    /// row, and the flap dismisses itself to reveal that row's detail.
    #[test]
    fn selecting_a_row_dismisses_the_overlay_flap() {
        let mut v = EditorView::seeded_sample(EditorTab::Roles, 0, 0);
        drive_frame(&mut v, 560, 800);
        let start = v.shared.borrow().selected_role;
        // Open the flap.
        let btn = v.menu_btn.center();
        editor_click(&mut v, btn);
        assert!(v.split.is_sidebar_open(), "hamburger opened the flap");
        drive_frame(&mut v, 560, 800); // settle the open flap + lay out its rows

        // Click the SECOND role row inside the overlay sidebar (flush-left,
        // TABBAR_H + sidebar HEADER_H + body pad, second ROW_H, dot region).
        let row1_y = TABBAR_H + 56.0 + 8.0 + 52.0 + 23.0;
        editor_click(&mut v, Point::new(16.0, row1_y));
        let now = v.shared.borrow().selected_role;
        assert!(now.is_some() && now != start, "clicking a different row changed the selection");
        assert!(!v.split.is_sidebar_open(), "selecting a row auto-dismissed the overlay flap");
    }

    #[test]
    fn sidebar_is_overlay_when_narrow_docked_when_wide() {
        let mut wide = EditorView::new();
        drive_frame(&mut wide, 1280, 800);
        assert_eq!(wide.split.layout_mode(), ui::widget::SplitLayout::Docked);

        let mut narrow = EditorView::new();
        drive_frame(&mut narrow, 520, 880);
        assert_eq!(narrow.split.layout_mode(), ui::widget::SplitLayout::Overlay);
    }

    #[test]
    fn new_role_button_creates_a_role() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        drive_frame(&mut v, 1280, 800);
        let before = v.set().roles.len();
        // Click the +New Role button via the real event path.
        let p = v.new_item.center();
        let mut handled = false;
        let mut ft = None;
        let mv = WindowEvent::CursorMoved { device_id: unsafe { std::mem::zeroed() }, position: winit::dpi::PhysicalPosition::new(p.x, p.y) };
        v.handle_event(&mv, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &TaffyTree::new());
        let ev = WindowEvent::MouseInput { device_id: unsafe { std::mem::zeroed() }, state: ElementState::Pressed, button: MouseButton::Left };
        v.handle_event(&ev, 1.0, &Default::default(), &mut doc, &ROOT, &mut handled, &mut ft, NodeId::new(0), &TaffyTree::new());
        assert_eq!(v.set().roles.len(), before + 1, "the +New Role button added a role");
    }
}
