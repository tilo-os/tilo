//! A bot's grants: the rules its program needs, held to the workspace it was
//! installed in, never reaching governance.
//!
//! A program's rules are written over any workspace, so one program can be
//! installed anywhere. Installing scopes them to one workspace. The grant ids
//! share one prefix per installation, so uninstalling revokes them all
//! without a list.

use std::fmt;

use crate::{Answer, Grant, Grantee, KindSet, Rule, TreePath, roles};

/// Why a program's rules cannot become a bot's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotRuleError {
    /// A rule names another workspace.
    OutsideWorkspace,
    /// A rule reaches the workspace resource, where governance lives.
    Governance,
    /// A rule asks or denies. Nobody is present to answer for a bot, and a
    /// deny in a program narrows nothing a derived grant does not already
    /// leave out.
    NotAllow,
}

impl fmt::Display for BotRuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutsideWorkspace => "a bot's grant names another workspace",
            Self::Governance => "a bot's grant never reaches the workspace's governance",
            Self::NotAllow => "a bot's grant only allows",
        })
    }
}

impl std::error::Error for BotRuleError {}

/// `rules` held to `workspace`: each node's workspace segment, named or any, becomes
/// `workspace`.
///
/// # Errors
/// The first rule that names another workspace, reaches the workspace resource, or
/// does not allow.
pub fn scoped_rules(workspace: &str, rules: &[Rule]) -> Result<Vec<Rule>, BotRuleError> {
    let workspace_node = TreePath::workspace(workspace);
    let governed = roles::workspace_resource(workspace);
    rules
        .iter()
        .map(|rule| {
            if rule.answer != Answer::Allow {
                return Err(BotRuleError::NotAllow);
            }
            if rule
                .node
                .workspace_id()
                .is_some_and(|named| named != workspace)
            {
                return Err(BotRuleError::OutsideWorkspace);
            }
            let node = workspace_node.join(&rule.node.within_workspace());
            if node.overlaps(&governed) {
                return Err(BotRuleError::Governance);
            }
            Ok(Rule::new(node, rule.kinds, rule.answer))
        })
        .collect()
}

#[must_use]
pub fn grant_prefix(installation: &str) -> String {
    format!("bot:{installation}:")
}

/// The grants installing `installation` in `workspace` gives, with `installer`
/// (a user root, hex) as the grantor: the scoped rules, then the deny on
/// the workspace resource that agent admissions carry too.
///
/// # Errors
/// See [`scoped_rules`].
pub fn grants(
    workspace: &str,
    installation: &str,
    installer: &str,
    rules: &[Rule],
) -> Result<Vec<Grant>, BotRuleError> {
    let subject = Grantee::bot(installation);
    let granted_by = Grantee::person(installer);
    let prefix = grant_prefix(installation);
    let mut grants: Vec<Grant> = scoped_rules(workspace, rules)?
        .into_iter()
        .enumerate()
        .map(|(n, rule)| {
            Grant::new(
                format!("{prefix}{n}"),
                subject.clone(),
                rule.node,
                rule.kinds,
                rule.answer,
                granted_by.clone(),
            )
        })
        .collect();
    grants.push(Grant::new(
        format!("{prefix}no-governance"),
        subject,
        roles::workspace_resource(workspace),
        KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        Answer::Deny,
        granted_by,
    ));
    Ok(grants)
}

/// Whether `new` allows nothing `old` does not: every allowing grant of
/// `new` sits at or below an allowing grant of `old` with at least its
/// kinds. Such an update needs no fresh privileged review.
#[must_use]
pub fn fits_within(old: &[Grant], new: &[Grant]) -> bool {
    new.iter()
        .filter(|grant| grant.answer == Answer::Allow)
        .all(|grant| {
            old.iter().any(|held| {
                held.answer == Answer::Allow
                    && held.node.covers(&grant.node)
                    && held.kinds.union(grant.kinds) == held.kinds
            })
        })
}
