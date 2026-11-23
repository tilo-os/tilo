//! Grant sets in plain language, for the people who give them and the agents
//! that hold them.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use crate::{ActionKind, Answer, Grant, Level, Lifetime, Segment, TreePath};

/// Names for nodes. The default uses ids; a host that knows app and resource
/// names supplies them, and the words the whole tree is called in the
/// person's language.
pub trait Labels {
    fn label(&self, level: Level, id: &str) -> Option<String> {
        let _ = (level, id);
        None
    }

    /// What the whole grant tree is called, in the words the person's
    /// language uses for it.
    fn everything(&self) -> String;
}

/// Labels nodes by their ids, for a reader that speaks the log's own
/// language (an agent, a bot's directory). A window passes its own labels,
/// so the word it paints for the whole tree comes from its catalogue.
#[derive(Clone, Copy, Debug, Default)]
pub struct Ids;

impl Labels for Ids {
    fn everything(&self) -> String {
        "Everything".to_owned()
    }
}

/// A node as people read it: `Mail › every resource › Send`.
#[must_use]
pub fn describe_node(node: &TreePath, labels: &dyn Labels) -> String {
    if node.is_root() {
        return labels.everything();
    }
    node.segments()
        .iter()
        .map(|segment| match segment {
            Segment::Name(level, id) => labels.label(*level, id).unwrap_or_else(|| match level {
                Level::Workspace => format!("workspace {id}"),
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
    // The longest-lived first under each node.
    let mut by_node: BTreeMap<(&TreePath, Reverse<Lifetime>), BTreeMap<ActionKind, Answer>> =
        BTreeMap::new();
    for grant in grants {
        let answers = by_node
            .entry((&grant.node, Reverse(grant.lifetime)))
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
        .map(|((node, Reverse(lifetime)), answers)| {
            format!(
                "{}{}: {}.",
                describe_node(node, labels),
                lifetime_suffix(lifetime),
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

/// What a sentence adds for a grant that lasts `lifetime`: nothing until revoked.
fn lifetime_suffix(lifetime: Lifetime) -> String {
    match lifetime {
        Lifetime::UntilRevoked => String::new(),
        Lifetime::Session => " (this session)".to_owned(),
        Lifetime::Until(at) => format!(" (until {at} ms)"),
        Lifetime::Once => " (once)".to_owned(),
    }
}

/// One sentence per grant, in the words of the person who gave it: where it
/// applies, then what it answers for the kinds of action it covers.
///
/// [`describe`] merges a grant set into what each node finally answers;
/// this keeps the grants apart, so a person reads the exception and the rule
/// it narrows as the two resources they are, and an agent handed the same
/// sentences knows what it holds. Grants come from the most specific node to
/// the least, so an exception sits beside the grant it excepts.
#[must_use]
pub fn sentences(grants: &[Grant], labels: &dyn Labels) -> String {
    if grants.is_empty() {
        return "Nothing is granted.".to_owned();
    }
    let mut ordered: Vec<&Grant> = grants.iter().collect();
    // Deepest first: what a person checks is the exception on top of the
    // broad grant below it.
    ordered.sort_by(|a, b| {
        b.node
            .segments()
            .len()
            .cmp(&a.node.segments().len())
            .then_with(|| b.node.cmp(&a.node))
            .then_with(|| a.id.cmp(&b.id))
    });
    ordered
        .iter()
        .map(|grant| {
            format!(
                "{}{}: {}.",
                describe_node(&grant.node, labels),
                lifetime_suffix(grant.lifetime),
                grant_clauses(grant),
            ) + if grant.denies_reading_within_workspace() {
                HIDDEN_NOT_SEALED
            } else {
                ""
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// What a sentence adds for a grant refusing reading inside a workspace: Tilo keeps it from them,
/// but their devices hold the workspace's keys ([`Grant::denies_reading_within_workspace`]).
const HIDDEN_NOT_SEALED: &str = " Tilo keeps it from them, but it is not encrypted apart: their devices hold the workspace's keys, so only the workspace itself keeps content from someone.";

/// What one grant answers for each kind of action it covers: "everything
/// allowed", "reading allowed", "undoable changes and outside effects allowed".
fn grant_clauses(grant: &Grant) -> String {
    if grant.kinds == crate::KindSet::ALL {
        return match grant.answer {
            Answer::Ask => "everything asks first".to_owned(),
            answer => format!("everything {}", answer.phrase()),
        };
    }
    let kinds: Vec<&str> = grant.kinds.iter().map(ActionKind::phrase).collect();
    let what = join_and(&kinds);
    if grant.answer == Answer::Deny {
        format!("{what} not allowed")
    } else {
        format!("{what} {}", grant.answer.phrase())
    }
}
