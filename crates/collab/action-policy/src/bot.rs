//! A bot's grants: the rules its program needs, held to the room it was
//! installed in, never reaching governance.
//!
//! A program's rules are written over any room, so one program can be
//! installed anywhere. Installing scopes them to one room. The grant ids
//! share one prefix per installation, so uninstalling revokes them all
//! without a list.

use std::fmt;

use crate::{Answer, Bound, Grant, KindSet, Rule, Subject, TreePath, roles};

/// Why a program's rules cannot become a bot's grant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BotRuleError {
    /// A rule names another room.
    OutsideRoom,
    /// A rule reaches the room resource, where governance lives.
    Governance,
    /// A rule asks or denies. Nobody is present to answer for a bot, and a
    /// deny in a program narrows nothing a derived grant does not already
    /// leave out.
    NotAllow,
}

impl fmt::Display for BotRuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::OutsideRoom => "a bot's grant names another room",
            Self::Governance => "a bot's grant never reaches the room's governance",
            Self::NotAllow => "a bot's grant only allows",
        })
    }
}

impl std::error::Error for BotRuleError {}

/// `rules` held to `room`: each node's room segment, named or any, becomes
/// `room`.
///
/// # Errors
/// The first rule that names another room, reaches the room resource, or
/// does not allow.
pub fn scoped_rules(room: &str, rules: &[Rule]) -> Result<Vec<Rule>, BotRuleError> {
    let room_node = TreePath::room(room);
    let governed = roles::room_resource(room);
    rules
        .iter()
        .map(|rule| {
            if rule.answer != Answer::Allow {
                return Err(BotRuleError::NotAllow);
            }
            if rule.node.room_id().is_some_and(|named| named != room) {
                return Err(BotRuleError::OutsideRoom);
            }
            let node = room_node.join(&rule.node.within_room());
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

/// The grants installing `installation` in `room` gives, with `installer`
/// (a user root, hex) as the grantor: the scoped rules, then the deny on
/// the room resource that agent admissions carry too.
///
/// # Errors
/// See [`scoped_rules`].
pub fn grants(
    room: &str,
    installation: &str,
    installer: &str,
    rules: &[Rule],
) -> Result<Vec<Grant>, BotRuleError> {
    let subject = Subject::bot(installation);
    let granted_by = Subject::person(installer);
    let prefix = grant_prefix(installation);
    let mut grants: Vec<Grant> = scoped_rules(room, rules)?
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
        roles::room_resource(room),
        KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        Answer::Deny,
        granted_by,
    ));
    Ok(grants)
}

/// The bound a bot acts within. Its grants live in the room's policy, so
/// the bound carries none of its own: [`Bound::party`] finds them there.
#[must_use]
pub fn bound(installation: &str) -> Bound {
    Bound {
        subject: Subject::bot(installation),
        grants: Vec::new(),
    }
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
