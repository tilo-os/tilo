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

/// The installation the moderation agent on a workspace's server acts as.
pub const MODERATION: &str = "moderation";

/// The id of the grant [`moderation_grant`] writes: one per workspace, so
/// turning moderation off revokes it by name.
pub const MODERATION_GRANT: &str = "bot:moderation";

/// The app the moderation agent acts in: the workspace's Chat, where reports are filed and
/// decided.
pub const MODERATED_APP: &str = tilo_app_id::apps::CHAT;

/// The grant a person gives the server's moderation agent when they turn it on
/// in `workspace`: reading and undoable changes in Chat ([`MODERATED_APP`]), where reports are
/// filed and decided, and nothing outside it. `person` is the grantor, a user
/// root, hex.
#[must_use]
pub fn moderation_grant(workspace: &str, person: &str) -> Grant {
    Grant::new(
        MODERATION_GRANT,
        Grantee::bot(MODERATION),
        TreePath::workspace(workspace).app(MODERATED_APP),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        Grantee::person(person.to_owned()),
    )
}

/// The governance verbs the moderation agent may take on the workspace's members: restrict one
/// from changing anything for a time, lift that, and ban or unban one from the workspace. Each
/// is the workspace's own action ([`roles::governance`]), decided at its own node.
pub const MODERATION_GOVERNANCE: [&str; 4] = [
    roles::governance::BAN,
    roles::governance::UNBAN,
    roles::governance::RESTRICT,
    roles::governance::UNRESTRICT,
];

/// The id of the grant letting the moderation agent take `verb` in `workspace`.
#[must_use]
pub fn moderation_governance_grant_id(verb: &str) -> String {
    format!("{MODERATION_GRANT}:{verb}")
}

/// Every grant a person gives the moderation agent when they turn it on in `workspace`: its
/// Chat grant ([`moderation_grant`]) and one for each governance verb it may take
/// ([`MODERATION_GOVERNANCE`]), at that verb's own node, given by `person` (a user root, hex).
#[must_use]
pub fn moderation_grants(workspace: &str, person: &str) -> Vec<Grant> {
    let granted_by = Grantee::person(person.to_owned());
    let mut grants = vec![moderation_grant(workspace, person)];
    grants.extend(MODERATION_GOVERNANCE.map(|verb| {
        Grant::new(
            moderation_governance_grant_id(verb),
            Grantee::bot(MODERATION),
            roles::governance_action(workspace, verb),
            KindSet::OUTSIDE,
            Answer::Allow,
            granted_by.clone(),
        )
    }));
    grants
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
    let grantee = Grantee::bot(installation);
    let granted_by = Grantee::person(installer);
    let prefix = grant_prefix(installation);
    let mut grants: Vec<Grant> = scoped_rules(workspace, rules)?
        .into_iter()
        .enumerate()
        .map(|(n, rule)| {
            Grant::new(
                format!("{prefix}{n}"),
                grantee.clone(),
                rule.node,
                rule.kinds,
                rule.answer,
                granted_by.clone(),
            )
        })
        .collect();
    grants.push(Grant::new(
        format!("{prefix}no-governance"),
        grantee,
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
