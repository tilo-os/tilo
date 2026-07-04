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

use crate::identity::IdentitySource;
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
    /// The real qOS identities members bind onto (see [`crate::identity`]).
    /// Resolves a member's stored identity id into its live display
    /// name/colour. In production a host injects the live roster via
    /// [`Self::set_identities`]; headless it is the sample identity model.
    pub identities: IdentitySource,

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
    /// Monotonic revision of everything that affects what the panes derive
    /// (the permission set, the selection, the active context, the tab, the
    /// identity roster). The detail pane's grid cache keys on this so it
    /// only rebuilds the (expensive) filtered + resolved grid when the
    /// revision actually moved — never per frame. Bumped by [`Self::bump`],
    /// which every mutation **and** every selection change calls.
    pub rev: u64,
}

/// A cheaply-cloned handle the two panes share.
pub type Shared = Rc<RefCell<EditorState>>;

impl EditorState {
    pub fn new(registry: PermissionRegistry, set: PermissionSet) -> Shared {
        Self::with_identities(registry, set, IdentitySource::sample_workspace())
    }

    /// Construct with an explicit identity source (the production path: a
    /// host passes the live collaboration roster as identities).
    pub fn with_identities(
        registry: PermissionRegistry,
        set: PermissionSet,
        identities: IdentitySource,
    ) -> Shared {
        Rc::new(RefCell::new(EditorState {
            registry,
            set,
            identities,
            tab: EditorTab::Roles,
            selected_role: None,
            selected_member: None,
            active_context: None,
            pending_write: false,
            dirty: true,
            rev: 0,
        }))
    }

    /// Bump the derived-state revision and request a redraw. Called by every
    /// mutation and every selection change so the panes' caches invalidate
    /// and [`EditorView`](super::EditorView) repaints. This is the single
    /// signal the whole editor keys its "something changed" off of.
    pub fn bump(&mut self) {
        self.rev = self.rev.wrapping_add(1);
        self.dirty = true;
    }

    /// Inject the live identity roster (production seam). The member views
    /// re-resolve names/colours through it on the next frame.
    pub fn set_identities(&mut self, identities: IdentitySource) {
        self.identities = identities;
        self.bump();
    }

    /// The display name to show for a member — resolved through the real
    /// identity source when the member is bound to an identity, falling back
    /// to the member's cached label otherwise.
    pub fn member_display_name(&self, member: &Member) -> String {
        match &member.identity_id {
            Some(id) => self.identities.display_name(id),
            None => member.display_name.clone(),
        }
    }

    /// The colour to show for a member — the bound identity's canonical
    /// colour (matching that peer's live cursor), or a stable hash colour.
    pub fn member_color(&self, member: &Member) -> u32 {
        match &member.identity_id {
            Some(id) => self.identities.color(id),
            None => crate::identity::color_for_id(&member.id.to_string()),
        }
    }

    /// Select a role and signal the change. **This is the fix for the
    /// "clicking another role doesn't switch" bug**: a bare
    /// `selected_role = Some(id)` assignment changed the model but never
    /// bumped the shared dirty/revision, so [`EditorView`](super::EditorView)
    /// never relaid-out or repainted the grid. Routing every selection
    /// through here guarantees the editor reacts.
    pub fn select_role(&mut self, id: RoleId) {
        if self.selected_role != Some(id) {
            self.selected_role = Some(id);
            self.bump();
        }
    }

    /// Select a member and signal the change (same fix as
    /// [`Self::select_role`]).
    pub fn select_member(&mut self, id: MemberId) {
        if self.selected_member != Some(id) {
            self.selected_member = Some(id);
            self.bump();
        }
    }

    /// Switch the active tab and signal the change.
    pub fn select_tab(&mut self, tab: EditorTab) {
        if self.tab != tab {
            self.tab = tab;
            self.bump();
        }
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
        self.bump();
    }

    /// Scope the grid to a dynamic context (or back to global) and signal
    /// the change so the grid cache rebuilds for the new scope.
    pub fn set_active_context(&mut self, ctx: Option<ContextId>) {
        if self.active_context != ctx {
            self.active_context = ctx;
            self.bump();
        }
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

    /// Add a member. Binds it to the first **real identity** from the source
    /// not already represented (so "+ New Member" picks an actual workspace
    /// peer), falling back to an unlinked placeholder only when every known
    /// identity already has a member.
    pub fn create_member(&mut self) -> MemberId {
        let used: std::collections::HashSet<&str> = self
            .set
            .members
            .iter()
            .filter_map(|m| m.identity_id.as_deref())
            .collect();
        let next_identity = self
            .identities
            .identities()
            .into_iter()
            .find(|i| !used.contains(i.id.as_str()))
            .cloned();
        let mem = match next_identity {
            Some(identity) => Member::for_identity(&identity),
            None => Member::new(format!("New Member {}", self.set.members.len() + 1)),
        };
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

#[cfg(test)]
mod identity_integration_tests {
    use super::*;
    use crate::identity::{Identity, IdentitySource};
    use crate::sample;

    fn state() -> Shared {
        let s = EditorState::new(sample::sample_registry(), sample::sample_set());
        s.borrow_mut().ensure_selection();
        s
    }

    #[test]
    fn sample_members_bind_to_real_identities() {
        let s = state();
        let st = s.borrow();
        // Every sample member references a REAL identity id (not a fabricated
        // free-text name), and that id resolves in the identity source.
        let bound = st.set.members.iter().filter(|m| m.identity_id.is_some()).count();
        assert!(bound >= 3, "sample members are real-identity bindings, not invented names");
        for m in &st.set.members {
            if let Some(id) = &m.identity_id {
                assert!(st.identities.get(id).is_some(), "member identity {id} exists in the source");
            }
        }
    }

    #[test]
    fn member_name_and_color_resolve_through_the_identity_source() {
        let s = state();
        let st = s.borrow();
        let m = st.set.members.iter().find(|m| m.identity_id.as_deref() == Some("alice")).expect("alice member");
        // Display name + colour come from the identity layer, and the colour
        // is the canonical presence colour for that peer id.
        assert_eq!(st.member_display_name(m), st.identities.display_name("alice"));
        assert_eq!(st.member_color(m), crate::identity::color_for_id("alice"));
    }

    #[test]
    fn live_roster_seam_overrides_names_and_colors() {
        // The production seam: a host injects the live collaboration roster.
        // After injection, members resolve to the LIVE roster name/colour.
        let s = state();
        let collab = ui::collaboration::Collaborator {
            id: "alice".into(),
            name: "Alice (live)".into(),
            color: [0.0, 1.0, 0.0],
        };
        s.borrow_mut().set_identities(IdentitySource::from_collaborators(std::slice::from_ref(&collab)));
        let st = s.borrow();
        let m = st.set.members.iter().find(|m| m.identity_id.as_deref() == Some("alice")).unwrap();
        assert_eq!(st.member_display_name(m), "Alice (live)", "live roster name wins");
        assert_eq!(st.member_color(m), 0x00_FF_00, "live roster colour wins");
    }

    #[test]
    fn select_role_bumps_revision_and_dirty() {
        // The bug-fix invariant: selecting a different role bumps the shared
        // revision + dirty flag so the editor reacts (relayout/redraw).
        let s = state();
        let order = s.borrow().visible_roles();
        s.borrow_mut().dirty = false;
        let rev0 = s.borrow().rev;
        s.borrow_mut().select_role(order[1]);
        assert!(s.borrow().dirty, "selecting a role marks dirty");
        assert!(s.borrow().rev > rev0, "selecting a role bumps the revision");
        // Re-selecting the same role is a no-op (no needless rebuild).
        s.borrow_mut().dirty = false;
        let rev1 = s.borrow().rev;
        s.borrow_mut().select_role(order[1]);
        assert!(!s.borrow().dirty, "re-selecting the same role is a no-op");
        assert_eq!(s.borrow().rev, rev1, "no revision bump on a no-op selection");
    }

    #[test]
    fn create_member_binds_next_real_identity() {
        let s = state();
        let before: std::collections::HashSet<String> =
            s.borrow().set.members.iter().filter_map(|m| m.identity_id.clone()).collect();
        let mid = s.borrow_mut().create_member();
        let st = s.borrow();
        let m = st.set.member(mid).unwrap();
        // A new member binds to a real identity not already used (when one is
        // available), not a fabricated "New Member N".
        if st.identities.len() > before.len() {
            assert!(m.identity_id.is_some(), "new member binds a real identity when one is free");
            assert!(!before.contains(m.identity_id.as_ref().unwrap()), "binds an UNUSED identity");
        }
    }

    #[test]
    fn identity_source_default_is_real_model_not_fabricated() {
        // The default headless source is the real identity model whose ids
        // are the collaboration layer's own canonical peer ids.
        let src = IdentitySource::sample_workspace();
        let alice = Identity::new("alice", "Alice");
        assert_eq!(src.color("alice"), alice.color);
    }
}
