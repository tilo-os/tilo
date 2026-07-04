//! The permission **registry**: the runtime catalogue of every
//! permission the system knows about.
//!
//! Apps and subsystems *register* the permissions they define at
//! startup. Each entry pins down the stable id, a human label, a longer
//! description, the [`Scope`] it belongs to, and the **default**
//! decision used when neither a role nor an override has an opinion.
//!
//! The registry is the dynamic half of the system: roles and overrides
//! (in [`crate::model`]) refer to permissions purely by id, so the
//! grantable set — and therefore what the editor shows — is exactly what
//! has been registered. Registration is code-driven and happens at
//! process start; it is deliberately *not* part of the user-authored
//! CRDT state, though a workspace may persist a snapshot for offline
//! peers (see the enforcement-seam notes in [`crate::seam`]).

use crate::model::{Decision, PermissionId, Scope};

/// One registered permission definition. This is the contract an app
/// publishes: "here is a thing that can be allowed or denied, here is
/// what it means, and here is what happens by default."
#[derive(Clone, PartialEq, Debug)]
pub struct PermissionDef {
    /// Stable, globally-unique id (e.g. `"chat.send_message"`).
    pub id: PermissionId,
    /// Short human label for the editor row (e.g. "Send Messages").
    pub label: String,
    /// One-line explanation shown in the editor.
    pub description: String,
    /// Which category this permission belongs to.
    pub scope: Scope,
    /// Decision used when no role and no override decides — the floor of
    /// the precedence chain.
    pub default: Decision,
}

impl PermissionDef {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        description: impl Into<String>,
        scope: Scope,
        default: Decision,
    ) -> Self {
        PermissionDef {
            id: id.into(),
            label: label.into(),
            description: description.into(),
            scope,
            default,
        }
    }
}

/// The catalogue. Insertion order is preserved within a scope so the
/// editor lists permissions in the order apps registered them. Lookups
/// by id are O(n) but the catalogue is small (tens to low hundreds of
/// entries) and lookups happen at check time against an in-memory map,
/// so a `Vec` keeps registration order without ceremony.
#[derive(Clone, Default, Debug)]
pub struct PermissionRegistry {
    defs: Vec<PermissionDef>,
}

impl PermissionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one permission. A later registration with the same id
    /// **replaces** the earlier one — last writer wins — so an app
    /// re-registering on hot-reload doesn't duplicate rows. Returns the
    /// id for convenient chaining.
    pub fn register(&mut self, def: PermissionDef) -> PermissionId {
        let id = def.id.clone();
        if let Some(slot) = self.defs.iter_mut().find(|d| d.id == def.id) {
            *slot = def;
        } else {
            self.defs.push(def);
        }
        id
    }

    /// Register a whole batch (an app's full permission set).
    pub fn register_all(&mut self, defs: impl IntoIterator<Item = PermissionDef>) {
        for d in defs {
            self.register(d);
        }
    }

    pub fn get(&self, id: &str) -> Option<&PermissionDef> {
        self.defs.iter().find(|d| d.id == id)
    }

    /// The registered default for `id`. Unknown permissions default to
    /// **Deny** — failing closed is the safe choice for a permission the
    /// system has never heard of.
    pub fn default_for(&self, id: &str) -> Decision {
        self.get(id).map(|d| d.default).unwrap_or(Decision::Deny)
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.defs.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PermissionDef> {
        self.defs.iter()
    }

    /// All definitions in a given scope, in registration order.
    pub fn in_scope(&self, scope: &Scope) -> Vec<&PermissionDef> {
        self.defs.iter().filter(|d| &d.scope == scope).collect()
    }

    /// Every distinct scope present, in first-seen order. Used by the
    /// editor to render group headers without hard-coding the scope list
    /// (so an app contributing a brand-new app-action group shows up).
    pub fn scopes(&self) -> Vec<Scope> {
        let mut seen: Vec<Scope> = Vec::new();
        for d in &self.defs {
            if !seen.contains(&d.scope) {
                seen.push(d.scope.clone());
            }
        }
        seen
    }

    /// Case-insensitive substring search over id + label + description,
    /// powering the editor's permission search box. Returns matches in
    /// registration order.
    pub fn search(&self, query: &str) -> Vec<&PermissionDef> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return self.defs.iter().collect();
        }
        self.defs
            .iter()
            .filter(|d| {
                d.id.to_lowercase().contains(&q)
                    || d.label.to_lowercase().contains(&q)
                    || d.description.to_lowercase().contains(&q)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_then_get() {
        let mut reg = PermissionRegistry::new();
        reg.register(PermissionDef::new(
            "workspace.manage",
            "Manage Workspace",
            "Rename, configure, and dissolve the workspace.",
            Scope::Workspace,
            Decision::Deny,
        ));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("workspace.manage").unwrap().label, "Manage Workspace");
        assert_eq!(reg.default_for("workspace.manage"), Decision::Deny);
    }

    #[test]
    fn unknown_permission_defaults_deny() {
        let reg = PermissionRegistry::new();
        assert_eq!(reg.default_for("nope.nonexistent"), Decision::Deny);
    }

    #[test]
    fn re_register_replaces_not_duplicates() {
        let mut reg = PermissionRegistry::new();
        reg.register(PermissionDef::new(
            "chat.send",
            "Send",
            "old",
            Scope::AppAction { app: "chat".into() },
            Decision::Deny,
        ));
        reg.register(PermissionDef::new(
            "chat.send",
            "Send Messages",
            "new",
            Scope::AppAction { app: "chat".into() },
            Decision::Allow,
        ));
        assert_eq!(reg.len(), 1, "same id must replace, not duplicate");
        assert_eq!(reg.get("chat.send").unwrap().label, "Send Messages");
        assert_eq!(reg.default_for("chat.send"), Decision::Allow);
    }

    #[test]
    fn scopes_are_first_seen_order() {
        let mut reg = PermissionRegistry::new();
        reg.register(PermissionDef::new("a", "A", "", Scope::Workspace, Decision::Deny));
        reg.register(PermissionDef::new("b", "B", "", Scope::Member, Decision::Deny));
        reg.register(PermissionDef::new("c", "C", "", Scope::Workspace, Decision::Deny));
        assert_eq!(reg.scopes(), vec![Scope::Workspace, Scope::Member]);
        assert_eq!(reg.in_scope(&Scope::Workspace).len(), 2);
    }

    #[test]
    fn search_matches_id_label_description() {
        let mut reg = PermissionRegistry::new();
        reg.register(PermissionDef::new(
            "chat.start_call",
            "Start Call",
            "Begin a voice or video call.",
            Scope::AppAction { app: "chat".into() },
            Decision::Allow,
        ));
        assert_eq!(reg.search("call").len(), 1);
        assert_eq!(reg.search("VOICE").len(), 1, "search hits the description");
        assert_eq!(reg.search("xyzzy").len(), 0);
        assert_eq!(reg.search("  ").len(), 1, "blank query returns everything");
    }
}
