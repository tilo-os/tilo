//! Roles over part of a workspace: a person given Files and nothing else, or one resource
//! shared with everyone.
//!
//! A member holds one entry per part they were given, each with who gave it. A whole-workspace
//! member holds one entry over the whole workspace ([`whole_workspace`]), which decides through
//! the role's own grants as it always did. Every other entry decides through grants derived
//! here: the role's rules ([`crate::roles::role_rules`]) cut to the entry's part, named
//! `scoped:<grantee>:<entry>:<rule>` and rebuilt from the entries whenever they change, never
//! given or taken back by hand ([`is_scoped_grant`]).

use serde::{Deserialize, Serialize};

use crate::{Grant, GrantId, Grantee, Role, Segment, TreePath, roles};
use tilo_action_types::Lifetime;

/// One role over one part of the workspace, and the person who gave it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedRole {
    pub scope: TreePath,
    pub role: Role,
    /// The giver, by person id: they take this entry back, as does whoever may revoke.
    /// Empty where the workspace recorded no giver.
    pub by: String,
}

/// Every role one person holds in a workspace. Never empty: losing the last entry is leaving.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub scopes: Vec<ScopedRole>,
}

/// The scope of a role over the whole workspace: `**`, whatever the workspace is called.
fn whole_workspace() -> TreePath {
    TreePath::default().rest()
}

/// Whether `scope` covers the whole workspace `workspace`.
#[must_use]
pub fn is_whole(scope: &TreePath, workspace: &str) -> bool {
    scope.segments() == [Segment::Rest] || scope.covers(&TreePath::workspace(workspace))
}

impl From<Role> for RoleAssignment {
    fn from(role: Role) -> Self {
        Self {
            scopes: vec![ScopedRole {
                scope: whole_workspace(),
                role,
                by: String::new(),
            }],
        }
    }
}

impl RoleAssignment {
    /// The role over the whole workspace, when there is one.
    #[must_use]
    pub fn whole(&self) -> Option<Role> {
        self.scopes
            .iter()
            .find(|entry| entry.scope.segments() == [Segment::Rest])
            .map(|entry| entry.role)
    }

    /// The role a listing shows: the whole workspace's, or else the highest over any part.
    #[must_use]
    pub fn role(&self) -> Role {
        self.whole().unwrap_or_else(|| {
            self.scopes
                .iter()
                .map(|entry| entry.role)
                .max_by_key(|role| Role::ALL.iter().position(|ranked| ranked == role))
                .unwrap_or(Role::Viewer)
        })
    }

    /// Set the role over the whole workspace, as `by` gives it.
    pub fn set_whole(&mut self, role: Role, by: &str) {
        self.scopes
            .retain(|entry| entry.scope.segments() != [Segment::Rest]);
        self.scopes.insert(
            0,
            ScopedRole {
                scope: whole_workspace(),
                role,
                by: by.to_owned(),
            },
        );
    }
}

/// Set or take back `by`'s entry over `scope` among `entries`. Taking back with `any` takes every
/// entry over `scope`, whoever gave it.
pub(crate) fn set_entry(
    entries: &mut Vec<ScopedRole>,
    scope: &TreePath,
    role: Option<Role>,
    by: &str,
    any: bool,
) {
    entries.retain(|entry| &entry.scope != scope || (entry.by != by && !any));
    if let Some(role) = role {
        let (scope, by) = (scope.clone(), by.to_owned());
        entries.push(ScopedRole { scope, role, by });
    }
}

/// Whether the entries hold one over `scope` that `by` gave.
pub(crate) fn gave(entries: &[ScopedRole], scope: &TreePath, by: &str) -> bool {
    (entries.iter()).any(|entry| &entry.scope == scope && entry.by == by)
}

/// The id prefix of every grant derived from an entry.
const PREFIX: &str = "scoped:";

/// Whether `grant` is derived from a scoped entry: the code's to write, never anyone's to give or
/// take back.
#[must_use]
pub fn is_scoped_grant(grant: &Grant) -> bool {
    grant.id.as_str().starts_with(PREFIX)
}

/// The grants `role` over `scope` in `workspace` gives `grantee`, under `scoped:<grantee>:<index>:`:
/// each of the role's rules cut to the scope, the rules outside it left out.
#[must_use]
pub fn grants(
    workspace: &str,
    (grantee_key, index): (&str, usize),
    entry: &ScopedRole,
    grantee: &Grantee,
) -> Vec<Grant> {
    let scope = &entry.scope;
    roles::role_rules(entry.role, workspace)
        .into_iter()
        .filter_map(|rule| {
            let node = if scope.covers(&rule.node) {
                rule.node
            } else if rule.node.covers(scope) {
                scope.clone()
            } else {
                return None;
            };
            Some((node, rule.kinds, rule.answer))
        })
        .enumerate()
        .map(|(at, (node, kinds, answer))| Grant {
            id: GrantId(format!("{PREFIX}{grantee_key}:{index}:{at}")),
            grantee: grantee.clone(),
            node,
            kinds,
            answer,
            lifetime: Lifetime::UntilRevoked,
            granted_by: if entry.by.is_empty() {
                grantee.clone()
            } else {
                Grantee::person(entry.by.clone())
            },
        })
        .collect()
}

/// The name everyone's entries are derived under.
pub(crate) const EVERYONE: &str = "everyone";
