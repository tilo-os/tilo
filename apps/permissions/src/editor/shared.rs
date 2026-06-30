//! Shared editor state — the single source of truth the sidebar and the
//! detail pane both read and mutate.
//!
//! The editor is split into two `Widget`s (a [`SidebarPane`] and a
//! [`DetailPane`]) so they can be composed by the toolkit's responsive
//! [`ui::widget::NavSplit`]. NavSplit owns its children as
//! `Box<dyn Widget>` and lays them out in an isolated inner tree, so the
//! two panes cannot hold `&mut` references to one shared struct. They
//! instead share an [`EditorState`] behind an `Rc<RefCell<…>>`: every
//! mutation (select, rename, reorder, toggle a grant) goes through this
//! struct, and [`EditorView`](super::EditorView) flushes it to the CRDT.
//!
//! [`SidebarPane`]: super::sidebar::SidebarPane
//! [`DetailPane`]: super::detail::DetailPane

use std::cell::RefCell;
use std::rc::Rc;

use crate::model::{
    ContextId, Member, MemberId, PermissionSet, Role, RoleId, Scope,
};
use crate::registry::{PermissionDef, PermissionRegistry};

/// Which screen the editor is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditorTab {
    Roles,
    Members,
}

/// A fixed palette the color picker offers and `create_role` cycles
/// through. Recognisable, evenly-spread hues.
pub const ROLE_PALETTE: [u32; 12] = [
    0xE6_4A_4A, 0xE6_8A_3A, 0xE6_C84A, 0x6FC74A, 0x4AC77A, 0x4AC7B8, 0x4A8FE6, 0x6A5AE6, 0x9A4AE6,
    0xE64AB8, 0x9A9A9A, 0x5A6472,
];

pub fn sample_color(i: usize) -> u32 {
    ROLE_PALETTE[i % ROLE_PALETTE.len()]
}

/// The mutable world both panes operate on. Selection + the active
/// context are *ephemeral* (view state, not persisted); the registry is
/// code-contributed; only [`set`](Self::set) is mirrored to the CRDT.
pub struct EditorState {
    pub registry: PermissionRegistry,
    pub set: PermissionSet,

    pub tab: EditorTab,
    pub selected_role: Option<RoleId>,
    pub selected_member: Option<MemberId>,
    /// Which dynamic context the per-instance grid is scoped to. `None` =
    /// the global grid (non-dynamic permissions).
    pub active_context: Option<ContextId>,

    /// Set when a structural edit needs to be flushed to the CRDT so peers
    /// converge. [`EditorView`](super::EditorView) drains this each frame.
    pub pending_write: bool,
    /// Bumped on any change that needs a redraw.
    pub dirty: bool,
}

/// A cheaply-cloned handle the two panes share.
pub type Shared = Rc<RefCell<EditorState>>;

impl EditorState {
    pub fn new(registry: PermissionRegistry, set: PermissionSet) -> Shared {
        Rc::new(RefCell::new(EditorState {
            registry,
            set,
            tab: EditorTab::Roles,
            selected_role: None,
            selected_member: None,
            active_context: None,
            pending_write: false,
            dirty: true,
        }))
    }

    // -----------------------------------------------------------------
    // Selection
    // -----------------------------------------------------------------

    /// The roles in *visible* order: **strongest first** (top of the
    /// sidebar). This order IS the precedence — the resolver
    /// ([`crate::check`]) walks `roles_of`, which sorts highest-position
    /// first, so the top role here is the one consulted first.
    pub fn visible_roles(&self) -> Vec<RoleId> {
        let mut rs: Vec<&Role> = self.set.roles.iter().collect();
        rs.sort_by(|a, b| b.position.cmp(&a.position));
        rs.into_iter().map(|r| r.id).collect()
    }

    pub fn visible_members(&self) -> Vec<MemberId> {
        let mut ms: Vec<&Member> = self.set.members.iter().collect();
        ms.sort_by(|a, b| a.display_name.cmp(&b.display_name));
        ms.into_iter().map(|m| m.id).collect()
    }

    pub fn ensure_selection(&mut self) {
        if self.selected_role.is_none() {
            self.selected_role = self.visible_roles().first().copied();
        }
        if self.selected_member.is_none() {
            self.selected_member = self.visible_members().first().copied();
        }
    }

    // -----------------------------------------------------------------
    // Grid permission set (honors the search box + active context)
    // -----------------------------------------------------------------

    /// Permissions to show in the right-hand grid, honoring the search
    /// query and the active context. When a context is active only the
    /// per-instance permissions matching that context's app+kind are
    /// shown; otherwise the non-dynamic ones are.
    pub fn grid_permissions(&self, query: &str) -> Vec<PermissionDef> {
        let active_ctx_meta = self
            .active_context
            .as_ref()
            .and_then(|id| self.set.context(id))
            .map(|c| (c.app.clone(), c.kind.clone()));
        self.registry
            .search(query)
            .into_iter()
            .filter(|d| match (&d.scope, &active_ctx_meta) {
                (Scope::DynamicInstance { app, kind }, Some((a, k))) => app == a && kind == k,
                (Scope::DynamicInstance { .. }, None) => false,
                (_, Some(_)) => false,
                (_, None) => true,
            })
            .cloned()
            .collect()
    }

    /// Group a permission list by scope, preserving registration order of
    /// the scopes.
    pub fn group_by_scope(perms: &[PermissionDef]) -> Vec<(Scope, Vec<PermissionDef>)> {
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

    pub fn mark_changed(&mut self) {
        self.pending_write = true;
        self.dirty = true;
    }

    pub fn create_role(&mut self) -> RoleId {
        let n = self.set.roles.len();
        let color = sample_color(n);
        let role = Role::new(format!("New Role {}", n + 1), color, self.set.next_position());
        let id = role.id;
        self.set.roles.push(role);
        self.selected_role = Some(id);
        self.tab = EditorTab::Roles;
        self.mark_changed();
        id
    }

    pub fn create_member(&mut self) -> MemberId {
        let mem = Member::new(format!("New Member {}", self.set.members.len() + 1));
        let id = mem.id;
        self.set.members.push(mem);
        self.selected_member = Some(id);
        self.tab = EditorTab::Members;
        self.mark_changed();
        id
    }

    pub fn delete_role(&mut self, id: RoleId) {
        self.set.remove_role(id);
        if self.selected_role == Some(id) {
            self.selected_role = self.visible_roles().first().copied();
        }
        self.mark_changed();
    }

    pub fn rename_role(&mut self, id: RoleId, name: &str) {
        if self.set.rename_role(id, name) {
            self.mark_changed();
        }
    }

    pub fn duplicate_role(&mut self, id: RoleId) -> Option<RoleId> {
        let new_id = self.set.duplicate_role(id);
        if let Some(nid) = new_id {
            self.selected_role = Some(nid);
            self.mark_changed();
        }
        new_id
    }

    /// Apply a new top-to-bottom (strongest-first) role order from a drag.
    pub fn set_role_order(&mut self, ordered: &[RoleId]) {
        self.set.set_role_order(ordered);
        self.mark_changed();
    }

    pub fn toggle_member_role(&mut self, member: MemberId, role: RoleId) {
        if let Some(m) = self.set.member_mut(member) {
            m.toggle_role(role);
            self.mark_changed();
        }
    }
}
