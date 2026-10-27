//! Grant sets in plain language, for the people who give them and the agents
//! that hold them.

use std::collections::BTreeMap;

use crate::{ActionKind, Answer, Grant, Level, Lifetime, Segment, TreePath};

/// Names for nodes. The default uses ids; a host that knows app and resource
/// names supplies them.
pub trait Labels {
    fn label(&self, level: Level, id: &str) -> Option<String> {
        let _ = (level, id);
        None
    }
}

/// Labels nodes by their ids.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ids;

impl Labels for Ids {}

/// A node as people read it: `Mail › every resource › Send`.
#[must_use]
pub fn describe_node(node: &TreePath, labels: &dyn Labels) -> String {
    if node.is_root() {
        return "Everything".to_owned();
    }
    node.segments()
        .iter()
        .map(|segment| match segment {
            Segment::Name(level, id) => labels.label(*level, id).unwrap_or_else(|| match level {
                Level::Room => format!("room {id}"),
                _ => id.clone(),
            }),
            Segment::Any(level) => format!("every {}", level.noun()),
            Segment::Rest => "anywhere".to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" › ")
}

/// One sentence per node: `Mail: reading and undoable changes allowed, outside
/// effects ask first.` Nodes come in tree order; grants with a lifetime other
/// than until-revoked say so.
#[must_use]
pub fn describe(grants: &[Grant], labels: &dyn Labels) -> String {
    let mut by_node: BTreeMap<(&TreePath, LifetimeKey<'_>), BTreeMap<ActionKind, Answer>> =
        BTreeMap::new();
    for grant in grants {
        let answers = by_node
            .entry((&grant.node, LifetimeKey(&grant.lifetime)))
            .or_default();
        for kind in grant.kinds.iter() {
            answers
                .entry(kind)
                .and_modify(|held| *held = (*held).min(grant.answer))
                .or_insert(grant.answer);
        }
    }
    if by_node.is_empty() {
        return "Nothing is granted.".to_owned();
    }
    by_node
        .into_iter()
        .map(|((node, lifetime), answers)| {
            format!(
                "{}{}: {}.",
                describe_node(node, labels),
                lifetime.suffix(),
                clauses(&answers)
            )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn clauses(answers: &BTreeMap<ActionKind, Answer>) -> String {
    if answers.len() == ActionKind::ALL.len() {
        let first = answers.values().next().copied();
        if let Some(answer) = first.filter(|answer| answers.values().all(|held| held == answer)) {
            return match answer {
                Answer::Ask => "everything asks first".to_owned(),
                answer => format!("everything {}", answer.phrase()),
            };
        }
    }
    [Answer::Allow, Answer::Ask, Answer::Deny]
        .into_iter()
        .filter_map(|answer| {
            let kinds: Vec<&str> = answers
                .iter()
                .filter(|(_, held)| **held == answer)
                .map(|(kind, _)| kind.phrase())
                .collect();
            (!kinds.is_empty()).then(|| format!("{} {}", join_and(&kinds), answer.phrase()))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Orders lifetimes for grouping without requiring `Ord` on the public type.
#[derive(PartialEq, Eq)]
struct LifetimeKey<'a>(&'a Lifetime);

impl LifetimeKey<'_> {
    fn rank(&self) -> (u8, &str, u64) {
        match self.0 {
            Lifetime::UntilRevoked => (0, "", 0),
            Lifetime::Session(session) => (1, session, 0),
            Lifetime::Until(at) => (3, "", *at),
            Lifetime::Once => (4, "", 0),
        }
    }

    fn suffix(&self) -> String {
        match self.0 {
            Lifetime::UntilRevoked => String::new(),
            Lifetime::Session(_) => " (this session)".to_owned(),
            Lifetime::Until(at) => format!(" (until {at} ms)"),
            Lifetime::Once => " (once)".to_owned(),
        }
    }
}

impl PartialOrd for LifetimeKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LifetimeKey<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}
