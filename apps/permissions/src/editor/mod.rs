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

    last_mouse: Point,
    seeded: bool,
    dirty: bool,
    relayout: bool,
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
            last_mouse: Point::ZERO,
            seeded: false,
            dirty: true,
            relayout: true,
        }
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
        self.tab_roles = Rect::new(r.x0 + PAD, tab_y, r.x0 + PAD + 96.0, tab_y + 32.0);
        self.tab_members = Rect::new(self.tab_roles.x1 + 8.0, tab_y, self.tab_roles.x1 + 8.0 + 112.0, tab_y + 32.0);
        self.new_item = Rect::new(r.x1 - PAD - 132.0, tab_y, r.x1 - PAD, tab_y + 32.0);
    }

    fn draw_tabbar(&self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let bar = Rect::new(self.rect.x0, self.rect.y0, self.rect.x1, self.rect.y0 + TABBAR_H);
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &bar);
        scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &Line::new((bar.x0, bar.y1), (bar.x1, bar.y1)));

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
            let existing = PermissionsDoc::read(state);
            {
                let mut s = self.shared.borrow_mut();
                if existing.roles.is_empty() && existing.members.is_empty() {
                    s.set = sample::sample_set();
                    PermissionsDoc::write(state, &s.set);
                } else {
                    s.set = existing;
                }
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

        // The split occupies everything below the tab bar. Build a tiny
        // inner taffy node sized to that band and lay the split into it.
        let split_rect = Rect::new(self.rect.x0, self.rect.y0 + TABBAR_H, self.rect.x1, self.rect.y1);
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
        // No reveal animation is driven by the live App loop for a split
        // rebuilt each frame, so snap to the resolved open state.
        self.split.snap_open(self.split.is_sidebar_open());

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
        self.split.handle_event(event, scale_factor, modifiers, state, path, handled, focus_target, node_id, taffy);

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
            let mut h_ = Harness { view: EditorView::new(), doc: AutoCommit::new(), w, h };
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

    #[test]
    fn seeds_sample_set_on_first_sync() {
        let mut v = EditorView::new();
        let mut doc = AutoCommit::new();
        v.sync(&mut doc, &ROOT);
        assert!(!v.set().roles.is_empty(), "sample roles seeded");
        assert!(v.shared.borrow().selected_role.is_some(), "a role auto-selected");
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
