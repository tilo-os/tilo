//! The permission data model: scopes, permission ids, roles, member
//! assignments, and per-context overrides.
//!
//! Everything here is plain data — no UI, no CRDT, no I/O — so the
//! resolution logic in [`crate::check`] can be exercised by pure unit
//! tests. The CRDT projection lives in [`crate::doc`]; it reads and
//! writes these same types.
//!
//! # Shape of the model
//!
//! ```text
//! PermissionSet              (the whole editable world, one per workspace)
//! ├── roles:    Vec<Role>        user-authored; NO defaults are baked in
//! ├── members:  Vec<Member>      who holds which roles + their overrides
//! └── contexts: Vec<Context>     named dynamic objects (a Chat server, …)
//!
//! Role
//! ├── id, name, color, position (hierarchy; higher = stronger)
//! └── grants:   Map<PermissionId, Grant>      Allow | Deny  (Inherit = absent)
//!
//! Member
//! ├── id, display_name, role_ids: Vec<RoleId>
//! └── overrides: Vec<Override>   explicit per-member Allow/Deny, optionally
//!                                scoped to a single Context
//! ```
//!
//! A [`PermissionId`] is *global and stable* (e.g. `chat.send_message`).
//! When that permission should be decided **per dynamic object** — say,
//! per Chat server — the *binding* to the object is carried by the
//! [`Context`] supplied at check time and by the optional
//! [`Override::context`] / [`RoleGrant`] context, never by mangling the
//! id. This keeps one registry entry (`chat.send_message`) reusable
//! across every server while still allowing a deny for one specific
//! server.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable, human-meaningful identifier for a single permission, e.g.
/// `"workspace.manage"` or `"chat.send_message"`. Apps choose these and
/// register them once at startup (see [`crate::registry`]); roles and
/// overrides refer to permissions only by this id, so the grantable set
/// is whatever has been registered.
pub type PermissionId = String;

/// Identifies a role. Roles are fully user-authored — the system ships
/// none — so ids are random UUIDs minted on creation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct RoleId(pub Uuid);

impl RoleId {
    pub fn new() -> Self {
        RoleId(Uuid::new_v4())
    }
}

impl std::fmt::Display for RoleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for RoleId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(RoleId(Uuid::parse_str(s)?))
    }
}

/// Identifies a workspace member (a person who may hold roles).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct MemberId(pub Uuid);

impl MemberId {
    pub fn new() -> Self {
        MemberId(Uuid::new_v4())
    }
}

impl std::fmt::Display for MemberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for MemberId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(MemberId(Uuid::parse_str(s)?))
    }
}

/// The categories the permission system spans. Modelled as a first-class
/// enum so the editor can group permissions and so an app's contributed
/// permissions land in the right bucket. `AppAction` carries the app's
/// own key (e.g. `"chat"`) so multiple apps coexist; `DynamicInstance`
/// marks permissions that are only meaningful relative to a specific
/// runtime object (a particular Chat server, a particular call).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Scope {
    /// Workspace-wide actions: manage the workspace, invite peers, …
    Workspace,
    /// Membership administration: add/remove/update members, assign roles.
    Member,
    /// App lifecycle: install / remove / update apps.
    AppManagement,
    /// A permission an app defines for its own features. `app` is the
    /// app's stable key so the editor can show "Chat", "Mail", …
    AppAction { app: String },
    /// A permission that is only decided in the context of a specific
    /// dynamic object instance — the canonical example being a single
    /// Chat server. `app` is the owning app; `kind` names the object
    /// type (e.g. `"server"`, `"channel"`). The *instance* itself is not
    /// part of the scope — it arrives as a [`Context`] at check time.
    DynamicInstance { app: String, kind: String },
}

impl Scope {
    /// A short, stable slug used for grouping + CRDT keys.
    pub fn slug(&self) -> String {
        match self {
            Scope::Workspace => "workspace".into(),
            Scope::Member => "member".into(),
            Scope::AppManagement => "app-management".into(),
            Scope::AppAction { app } => format!("app:{app}"),
            Scope::DynamicInstance { app, kind } => format!("dyn:{app}:{kind}"),
        }
    }

    /// Human label for the editor's group headers.
    pub fn label(&self) -> String {
        match self {
            Scope::Workspace => "Workspace".into(),
            Scope::Member => "Members".into(),
            Scope::AppManagement => "App Management".into(),
            Scope::AppAction { app } => format!("{} actions", title_case(app)),
            Scope::DynamicInstance { app, kind } => {
                format!("{} · per-{kind}", title_case(app))
            }
        }
    }

    /// True when grants/overrides for this scope's permissions are only
    /// meaningful relative to a [`Context`] object instance.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Scope::DynamicInstance { .. })
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// A three-state grant on a single permission. `Inherit` is the absence
/// of an explicit decision — the resolver falls through to the next
/// authority (lower roles, then the registered default).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Grant {
    Allow,
    Deny,
    /// No opinion — defer to the next authority in precedence.
    Inherit,
}

impl Grant {
    pub fn label(&self) -> &'static str {
        match self {
            Grant::Allow => "Allow",
            Grant::Deny => "Deny",
            Grant::Inherit => "Inherit",
        }
    }

    /// Cycle Inherit → Allow → Deny → Inherit, for the editor's tri-state
    /// toggle when a user clicks the same cell repeatedly.
    pub fn next(self) -> Grant {
        match self {
            Grant::Inherit => Grant::Allow,
            Grant::Allow => Grant::Deny,
            Grant::Deny => Grant::Inherit,
        }
    }
}

/// The final answer from [`crate::check::check`]. Deliberately binary:
/// enforcement sites either proceed or refuse. `Inherit` never escapes
/// the resolver — it is always collapsed to Allow/Deny via the default.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    Deny,
}

impl Decision {
    pub fn allowed(self) -> bool {
        matches!(self, Decision::Allow)
    }
}

/// A user-authored role. The system ships none of these — the user
/// creates every role.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    /// 8-bit RGB color used as the role's accent in the editor and as
    /// the member-list color. Stored as a packed `0xRRGGBB`.
    pub color: u32,
    /// Hierarchy position. Higher position = higher in the hierarchy =
    /// resolved *last* so it can override lower roles. Positions are kept
    /// unique and contiguous by the editor but the resolver only relies
    /// on the ordering.
    pub position: i64,
    /// Explicit grants this role makes, keyed by permission id. A
    /// permission absent from the map (or present as `Inherit`) means the
    /// role expresses no opinion on it.
    pub grants: Vec<RoleGrant>,
}

/// One grant a role makes on one permission, optionally narrowed to a
/// single dynamic context. A `context: None` grant applies everywhere
/// the permission is checked; a `context: Some(id)` grant applies only
/// when the check carries that same context (e.g. "this role may manage
/// channels, but only in the #engineering server").
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RoleGrant {
    pub permission: PermissionId,
    pub grant: Grant,
    /// `None` = global; `Some(ctx)` = only within that context instance.
    pub context: Option<ContextId>,
}

impl Role {
    pub fn new(name: impl Into<String>, color: u32, position: i64) -> Self {
        Role {
            id: RoleId::new(),
            name: name.into(),
            color,
            position,
            grants: Vec::new(),
        }
    }

    /// The grant this role expresses for `permission` within `context`.
    /// A context-specific grant takes priority over a global one for the
    /// same permission; otherwise the global grant applies; otherwise
    /// `Inherit`.
    pub fn grant_for(&self, permission: &str, context: Option<&ContextId>) -> Grant {
        let mut global = Grant::Inherit;
        let mut scoped = None;
        for g in &self.grants {
            if g.permission != permission {
                continue;
            }
            match (&g.context, context) {
                (Some(c), Some(want)) if c == want => scoped = Some(g.grant),
                (None, _) => global = g.grant,
                _ => {}
            }
        }
        scoped.unwrap_or(global)
    }

    /// Set (or clear, via `Inherit`) this role's grant for a permission
    /// within an optional context. Replaces any existing matching grant.
    pub fn set_grant(&mut self, permission: &str, context: Option<ContextId>, grant: Grant) {
        self.grants
            .retain(|g| !(g.permission == permission && g.context == context));
        if grant != Grant::Inherit {
            self.grants.push(RoleGrant {
                permission: permission.to_string(),
                grant,
                context,
            });
        }
    }
}

/// Identifies a dynamic object instance that permissions can be scoped
/// to (a specific Chat server, a specific call). Stored as an opaque
/// string so any app can mint ids in whatever shape it likes
/// (`"chat:server:1a2b"`).
#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct ContextId(pub String);

impl ContextId {
    pub fn new(s: impl Into<String>) -> Self {
        ContextId(s.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ContextId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named dynamic object instance — the binding target for
/// per-instance permissions. The editor lists these so a user can pick
/// "edit overrides for the #engineering server". `kind`/`app` mirror the
/// owning [`Scope::DynamicInstance`] so the editor can show only the
/// relevant per-instance permissions when one is selected.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Context {
    pub id: ContextId,
    pub app: String,
    pub kind: String,
    pub label: String,
}

impl Context {
    pub fn new(
        id: impl Into<String>,
        app: impl Into<String>,
        kind: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Context {
            id: ContextId::new(id),
            app: app.into(),
            kind: kind.into(),
            label: label.into(),
        }
    }
}

/// An explicit, per-member decision applied *on top of* whatever the
/// member's roles resolve to — the Discord "member override" concept.
/// Optionally narrowed to a single context.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Override {
    pub permission: PermissionId,
    pub grant: Grant,
    /// `None` = applies wherever the permission is checked for this
    /// member; `Some(ctx)` = only within that context instance.
    pub context: Option<ContextId>,
}

/// A workspace member: a person who holds zero or more roles and may
/// carry individual overrides.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Member {
    pub id: MemberId,
    pub display_name: String,
    pub role_ids: Vec<RoleId>,
    pub overrides: Vec<Override>,
}

impl Member {
    pub fn new(name: impl Into<String>) -> Self {
        Member {
            id: MemberId::new(),
            display_name: name.into(),
            role_ids: Vec::new(),
            overrides: Vec::new(),
        }
    }

    pub fn has_role(&self, role: RoleId) -> bool {
        self.role_ids.contains(&role)
    }

    pub fn toggle_role(&mut self, role: RoleId) {
        if let Some(i) = self.role_ids.iter().position(|r| *r == role) {
            self.role_ids.remove(i);
        } else {
            self.role_ids.push(role);
        }
    }

    /// The override grant for `permission` within `context`, applying the
    /// same context-beats-global rule as [`Role::grant_for`].
    pub fn override_for(&self, permission: &str, context: Option<&ContextId>) -> Grant {
        let mut global = Grant::Inherit;
        let mut scoped = None;
        for o in &self.overrides {
            if o.permission != permission {
                continue;
            }
            match (&o.context, context) {
                (Some(c), Some(want)) if c == want => scoped = Some(o.grant),
                (None, _) => global = o.grant,
                _ => {}
            }
        }
        scoped.unwrap_or(global)
    }

    pub fn set_override(&mut self, permission: &str, context: Option<ContextId>, grant: Grant) {
        self.overrides
            .retain(|o| !(o.permission == permission && o.context == context));
        if grant != Grant::Inherit {
            self.overrides.push(Override {
                permission: permission.to_string(),
                grant,
                context,
            });
        }
    }
}

/// The whole editable permission world for a workspace: roles, members,
/// and the dynamic contexts. The *registry* of permission definitions is
/// kept separately (see [`crate::registry`]) because definitions are
/// contributed by code at startup, whereas everything here is authored
/// by users and synced via the CRDT.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct PermissionSet {
    pub roles: Vec<Role>,
    pub members: Vec<Member>,
    pub contexts: Vec<Context>,
}

impl PermissionSet {
    pub fn role(&self, id: RoleId) -> Option<&Role> {
        self.roles.iter().find(|r| r.id == id)
    }
    pub fn role_mut(&mut self, id: RoleId) -> Option<&mut Role> {
        self.roles.iter_mut().find(|r| r.id == id)
    }
    pub fn member(&self, id: MemberId) -> Option<&Member> {
        self.members.iter().find(|m| m.id == id)
    }
    pub fn member_mut(&mut self, id: MemberId) -> Option<&mut Member> {
        self.members.iter_mut().find(|m| m.id == id)
    }
    pub fn context(&self, id: &ContextId) -> Option<&Context> {
        self.contexts.iter().find(|c| &c.id == id)
    }

    /// Roles a member holds, returned **highest-position-first** so a
    /// caller iterating them sees the strongest role first.
    pub fn roles_of(&self, member: &Member) -> Vec<&Role> {
        let mut rs: Vec<&Role> = member
            .role_ids
            .iter()
            .filter_map(|id| self.role(*id))
            .collect();
        rs.sort_by(|a, b| b.position.cmp(&a.position));
        rs
    }

    /// The next free hierarchy position (one above the current top).
    pub fn next_position(&self) -> i64 {
        self.roles.iter().map(|r| r.position).max().unwrap_or(0) + 1
    }

    /// Move a role one step up (`delta = +1`) or down (`delta = -1`) in
    /// the hierarchy by swapping positions with its neighbour.
    pub fn reorder(&mut self, id: RoleId, delta: i64) {
        let mut order: Vec<RoleId> = {
            let mut rs: Vec<&Role> = self.roles.iter().collect();
            rs.sort_by(|a, b| a.position.cmp(&b.position));
            rs.into_iter().map(|r| r.id).collect()
        };
        let Some(idx) = order.iter().position(|r| *r == id) else {
            return;
        };
        let target = idx as i64 + delta;
        if target < 0 || target as usize >= order.len() {
            return;
        }
        order.swap(idx, target as usize);
        // Reassign contiguous positions bottom-up.
        for (pos, rid) in order.iter().enumerate() {
            if let Some(r) = self.role_mut(*rid) {
                r.position = pos as i64;
            }
        }
    }

    /// Impose an explicit top-to-bottom precedence order on the roles.
    ///
    /// `ordered` is the *visible* order with the **strongest role first**
    /// (top of the sidebar). Positions are reassigned so that the first
    /// entry gets the highest position and the last gets the lowest —
    /// making the visible order *be* the precedence the resolver honors
    /// ([`roles_of`](Self::roles_of) sorts highest-position-first). Any
    /// role not present in `ordered` keeps its relative order below the
    /// listed ones (defensive; the editor always passes the full set).
    pub fn set_role_order(&mut self, ordered: &[RoleId]) {
        // Roles not named in `ordered` are appended (lowest), preserving
        // their existing relative order so a partial list never drops a
        // role's precedence to an arbitrary value.
        let mut full: Vec<RoleId> = ordered.to_vec();
        let mut leftovers: Vec<&Role> = self
            .roles
            .iter()
            .filter(|r| !ordered.contains(&r.id))
            .collect();
        leftovers.sort_by(|a, b| b.position.cmp(&a.position));
        full.extend(leftovers.into_iter().map(|r| r.id));

        // First listed = strongest = highest position. Assign descending
        // contiguous positions from the top.
        let top = full.len() as i64 - 1;
        for (i, rid) in full.iter().enumerate() {
            if let Some(r) = self.role_mut(*rid) {
                r.position = top - i as i64;
            }
        }
    }

    /// Duplicate a role: a deep copy with a fresh id, a "(copy)" name, and
    /// a position **just below** the original so it lands directly under
    /// it in the visible list. All grants (including context-scoped ones)
    /// are copied. Returns the new role's id, or `None` if `id` is unknown.
    pub fn duplicate_role(&mut self, id: RoleId) -> Option<RoleId> {
        let src = self.role(id)?.clone();
        let new_id = RoleId::new();
        let mut copy = Role {
            id: new_id,
            name: format!("{} (copy)", src.name),
            color: src.color,
            position: src.position, // temporary; fixed up below
            grants: src.grants.clone(),
        };
        // Insert just below the original in precedence: shift everyone
        // *strictly below* the source down by one (the source itself stays
        // put), opening the slot at `src_pos - 1` for the copy.
        let src_pos = src.position;
        for r in &mut self.roles {
            if r.position < src_pos {
                r.position -= 1;
            }
        }
        copy.position = src_pos - 1;
        self.roles.push(copy);
        Some(new_id)
    }

    pub fn remove_role(&mut self, id: RoleId) {
        self.roles.retain(|r| r.id != id);
        for m in &mut self.members {
            m.role_ids.retain(|r| *r != id);
        }
    }

    /// Rename a role in place. Returns `false` if the id is unknown.
    pub fn rename_role(&mut self, id: RoleId, name: impl Into<String>) -> bool {
        if let Some(r) = self.role_mut(id) {
            r.name = name.into();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod model_order_tests {
    use super::*;

    fn roles3() -> (PermissionSet, RoleId, RoleId, RoleId) {
        let mut set = PermissionSet::default();
        let a = Role::new("A", 0, 2);
        let b = Role::new("B", 0, 1);
        let c = Role::new("C", 0, 0);
        let (ai, bi, ci) = (a.id, b.id, c.id);
        set.roles.extend([a, b, c]);
        (set, ai, bi, ci)
    }

    #[test]
    fn set_role_order_makes_visible_order_the_precedence() {
        let (mut set, a, b, c) = roles3();
        // Reorder to C, A, B (top-to-bottom, strongest first).
        set.set_role_order(&[c, a, b]);
        // C is now strongest (highest position), B weakest.
        assert!(set.role(c).unwrap().position > set.role(a).unwrap().position);
        assert!(set.role(a).unwrap().position > set.role(b).unwrap().position);
    }

    #[test]
    fn duplicate_role_inserts_a_copy_just_below_original() {
        let (mut set, a, _b, _c) = roles3();
        let before = set.roles.len();
        let copy = set.duplicate_role(a).unwrap();
        assert_eq!(set.roles.len(), before + 1);
        assert_eq!(set.role(copy).unwrap().name, "A (copy)");
        // The copy sits directly below the original in precedence.
        assert!(set.role(a).unwrap().position > set.role(copy).unwrap().position);
        // Nothing else is now between them.
        let gap = set.role(a).unwrap().position - set.role(copy).unwrap().position;
        assert_eq!(gap, 1, "copy lands immediately under the source");
    }

    #[test]
    fn rename_role_changes_the_name() {
        let (mut set, a, _b, _c) = roles3();
        assert!(set.rename_role(a, "Renamed"));
        assert_eq!(set.role(a).unwrap().name, "Renamed");
    }
}
