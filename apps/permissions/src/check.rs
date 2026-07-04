//! The check API and its precedence algorithm.
//!
//! [`check`] is a pure function — given the registry, the permission
//! set, a member, a permission id, and an optional context, it returns a
//! single [`Decision`]. No mutation, no I/O; the editor and any
//! enforcement site call the exact same function.
//!
//! # Precedence (highest authority first)
//!
//! 1. **Member override** — an explicit `Allow`/`Deny` the user pinned on
//!    this member. A context-specific override beats a global one.
//! 2. **Roles, by hierarchy** — walk the member's roles from
//!    highest-position to lowest. The first role that expresses an
//!    opinion (`Allow` or `Deny`, again preferring a context-specific
//!    grant) decides. Higher roles therefore override lower ones.
//! 3. **Registered default** — if nothing above decided, fall through to
//!    the permission's registered default. Unknown permissions fail
//!    closed (`Deny`).
//!
//! This mirrors Discord: per-member overrides sit on top of the union of
//! roles, and role hierarchy breaks ties between roles. A `Deny` from a
//! higher role beats an `Allow` from a lower role because the higher
//! role is consulted first and short-circuits.
//!
//! The per-context dimension threads through every layer: a permission
//! registered as a [`crate::model::Scope::DynamicInstance`] is always
//! checked *with* a context, and grants/overrides may be pinned to that
//! one context. A context-scoped grant is strictly more specific than a
//! global grant for the same permission and so is consulted first within
//! its layer.

use crate::model::{ContextId, Decision, Grant, Member, PermissionSet};
use crate::registry::PermissionRegistry;

/// Why a decision came out the way it did — surfaced in the editor's
/// per-member preview so a user can see *which* authority decided. Not
/// needed by enforcement sites (they only care about the [`Decision`]),
/// but invaluable for explaining "effective permissions".
#[derive(Clone, PartialEq, Debug)]
pub enum Source {
    /// An explicit override on the member settled it.
    MemberOverride,
    /// A role settled it; carries the role's name for display.
    Role(String),
    /// Nothing decided; the registered default applied.
    Default,
}

/// A decision plus the authority that produced it.
#[derive(Clone, PartialEq, Debug)]
pub struct Resolution {
    pub decision: Decision,
    pub source: Source,
}

/// Resolve a single permission for a member, returning just the
/// [`Decision`]. This is the function enforcement sites call.
pub fn check(
    registry: &PermissionRegistry,
    set: &PermissionSet,
    member: &Member,
    permission: &str,
    context: Option<&ContextId>,
) -> Decision {
    explain(registry, set, member, permission, context).decision
}

/// Like [`check`], but also reports the deciding authority. The editor
/// uses this to render "effective" badges and a source hint.
pub fn explain(
    registry: &PermissionRegistry,
    set: &PermissionSet,
    member: &Member,
    permission: &str,
    context: Option<&ContextId>,
) -> Resolution {
    // 1. Member override — the top authority.
    match member.override_for(permission, context) {
        Grant::Allow => {
            return Resolution {
                decision: Decision::Allow,
                source: Source::MemberOverride,
            };
        }
        Grant::Deny => {
            return Resolution {
                decision: Decision::Deny,
                source: Source::MemberOverride,
            };
        }
        Grant::Inherit => {}
    }

    // 2. Roles, strongest first. First opinion wins.
    for role in set.roles_of(member) {
        match role.grant_for(permission, context) {
            Grant::Allow => {
                return Resolution {
                    decision: Decision::Allow,
                    source: Source::Role(role.name.clone()),
                };
            }
            Grant::Deny => {
                return Resolution {
                    decision: Decision::Deny,
                    source: Source::Role(role.name.clone()),
                };
            }
            Grant::Inherit => {}
        }
    }

    // 3. Registered default (Deny for unknown ids).
    Resolution {
        decision: registry.default_for(permission),
        source: Source::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Decision, Grant, Member, Role, Scope};
    use crate::registry::PermissionDef;

    fn reg_with(id: &str, default: Decision) -> PermissionRegistry {
        let mut r = PermissionRegistry::new();
        r.register(PermissionDef::new(id, id, "", Scope::Workspace, default));
        r
    }

    #[test]
    fn falls_through_to_registered_default_when_nothing_decides() {
        let reg = reg_with("p", Decision::Allow);
        let set = PermissionSet::default();
        let m = Member::new("Ada");
        let res = explain(&reg, &set, &m, "p", None);
        assert_eq!(res.decision, Decision::Allow);
        assert_eq!(res.source, Source::Default);
    }

    #[test]
    fn unknown_permission_fails_closed() {
        let reg = PermissionRegistry::new();
        let set = PermissionSet::default();
        let m = Member::new("Ada");
        assert_eq!(check(&reg, &set, &m, "never.registered", None), Decision::Deny);
    }

    #[test]
    fn role_grant_overrides_default() {
        // Default denies; a held role allows.
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let mut role = Role::new("Mod", 0xff0000, 1);
        role.set_grant("p", None, Grant::Allow);
        let rid = role.id;
        set.roles.push(role);
        let mut m = Member::new("Ada");
        m.role_ids.push(rid);
        set.members.push(m.clone());
        let res = explain(&reg, &set, &m, "p", None);
        assert_eq!(res.decision, Decision::Allow);
        assert_eq!(res.source, Source::Role("Mod".into()));
    }

    #[test]
    fn higher_role_deny_beats_lower_role_allow() {
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let mut low = Role::new("Member", 0x00ff00, 1);
        low.set_grant("p", None, Grant::Allow);
        let mut high = Role::new("Muted", 0x888888, 5);
        high.set_grant("p", None, Grant::Deny);
        let (lid, hid) = (low.id, high.id);
        set.roles.push(low);
        set.roles.push(high);
        let mut m = Member::new("Ada");
        m.role_ids.push(lid);
        m.role_ids.push(hid);
        let res = explain(&reg, &set, &m, "p", None);
        assert_eq!(res.decision, Decision::Deny, "higher role wins the tie");
        assert_eq!(res.source, Source::Role("Muted".into()));
    }

    #[test]
    fn lower_role_allow_applies_when_higher_role_inherits() {
        // The higher role expresses no opinion on `p`; the lower role's
        // Allow must still take effect rather than falling to default.
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let mut low = Role::new("Member", 0x00ff00, 1);
        low.set_grant("p", None, Grant::Allow);
        let high = Role::new("Decorative", 0x888888, 5); // no grant on p
        let (lid, hid) = (low.id, high.id);
        set.roles.push(low);
        set.roles.push(high);
        let mut m = Member::new("Ada");
        m.role_ids.push(lid);
        m.role_ids.push(hid);
        assert_eq!(check(&reg, &set, &m, "p", None), Decision::Allow);
    }

    #[test]
    fn member_override_beats_every_role() {
        // Roles all deny; the member override allows and must win.
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let mut high = Role::new("Muted", 0x888888, 9);
        high.set_grant("p", None, Grant::Deny);
        let hid = high.id;
        set.roles.push(high);
        let mut m = Member::new("Ada");
        m.role_ids.push(hid);
        m.set_override("p", None, Grant::Allow);
        let res = explain(&reg, &set, &m, "p", None);
        assert_eq!(res.decision, Decision::Allow);
        assert_eq!(res.source, Source::MemberOverride);
    }

    #[test]
    fn context_scoped_role_grant_beats_global_role_grant() {
        // The role globally allows `p`, but denies it inside one server.
        let reg = reg_with("p", Decision::Allow);
        let mut set = PermissionSet::default();
        let ctx = ContextId::new("chat:server:eng");
        let mut role = Role::new("Member", 0x00ff00, 1);
        role.set_grant("p", None, Grant::Allow);
        role.set_grant("p", Some(ctx.clone()), Grant::Deny);
        let rid = role.id;
        set.roles.push(role);
        let mut m = Member::new("Ada");
        m.role_ids.push(rid);
        // Global check: allowed.
        assert_eq!(check(&reg, &set, &m, "p", None), Decision::Allow);
        // In the eng server: denied by the context-specific grant.
        assert_eq!(check(&reg, &set, &m, "p", Some(&ctx)), Decision::Deny);
        // A different server is unaffected by the eng-specific grant.
        let other = ContextId::new("chat:server:design");
        assert_eq!(check(&reg, &set, &m, "p", Some(&other)), Decision::Allow);
    }

    #[test]
    fn context_scoped_member_override_beats_context_scoped_role() {
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let ctx = ContextId::new("chat:server:eng");
        let mut role = Role::new("Member", 0x00ff00, 1);
        role.set_grant("p", Some(ctx.clone()), Grant::Deny);
        let rid = role.id;
        set.roles.push(role);
        let mut m = Member::new("Ada");
        m.role_ids.push(rid);
        m.set_override("p", Some(ctx.clone()), Grant::Allow);
        let res = explain(&reg, &set, &m, "p", Some(&ctx));
        assert_eq!(res.decision, Decision::Allow);
        assert_eq!(res.source, Source::MemberOverride);
    }

    #[test]
    fn global_override_applies_inside_a_context_when_no_context_override() {
        // A global member override with no context-specific opinion still
        // applies when checking within a context.
        let reg = reg_with("p", Decision::Deny);
        let mut set = PermissionSet::default();
        let ctx = ContextId::new("chat:server:eng");
        let mut m = Member::new("Ada");
        m.set_override("p", None, Grant::Allow);
        set.members.push(m.clone());
        assert_eq!(check(&reg, &set, &m, "p", Some(&ctx)), Decision::Allow);
    }

    #[test]
    fn member_with_no_roles_uses_default() {
        let reg = reg_with("p", Decision::Allow);
        let set = PermissionSet::default();
        let m = Member::new("Guest");
        assert_eq!(check(&reg, &set, &m, "p", None), Decision::Allow);
    }

    #[test]
    fn setting_grant_to_inherit_clears_it() {
        let mut role = Role::new("Mod", 0, 1);
        role.set_grant("p", None, Grant::Allow);
        assert_eq!(role.grant_for("p", None), Grant::Allow);
        role.set_grant("p", None, Grant::Inherit);
        assert_eq!(role.grant_for("p", None), Grant::Inherit);
        assert!(role.grants.is_empty(), "Inherit must remove the stored grant");
    }
}
