//! The one decision every door, host and receiver makes.

use crate::{ActionKind, Answer, Context, Grant, Lifetime, Subject, TreePath};

/// Why a party answered as it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis<'g> {
    /// The most specific live grant covering the node and kind.
    Grant(&'g Grant),
    /// The party owns the room the node is in.
    Owner,
    /// No live grant covers the node and kind.
    NoGrant,
}

/// One party's answer for one node and kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decision<'g> {
    pub answer: Answer,
    pub basis: Basis<'g>,
}

impl<'g> Decision<'g> {
    #[must_use]
    pub const fn grant(&self) -> Option<&'g Grant> {
        match self.basis {
            Basis::Grant(grant) => Some(grant),
            _ => None,
        }
    }
}

/// Decides for one set of subjects over one set of grants:
///
/// 1. Take the live grants whose subject is one of `subjects`, whose node is
///    `node` or an ancestor of it, and whose kinds include `kind`.
/// 2. The most specific node wins.
/// 3. At the same node, Deny beats Ask beats Allow.
/// 4. No grant: Deny.
pub fn decide<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    subjects: &[Subject],
    node: &TreePath,
    kind: ActionKind,
    cx: &Context<'_>,
) -> Decision<'g> {
    let mut best: Option<(crate::path::Specificity, &'g Grant)> = None;
    for grant in grants {
        if !grant.kinds.contains(kind)
            || !subjects.contains(&grant.subject)
            || !grant.node.covers(node)
            || !grant.is_live(cx)
        {
            continue;
        }
        let specificity = grant.node.specificity();
        let better = best.as_ref().is_none_or(|(held, held_grant)| {
            specificity > *held || (specificity == *held && grant.answer < held_grant.answer)
        });
        if better {
            best = Some((specificity, grant));
        }
    }
    best.map_or(
        Decision {
            answer: Answer::Deny,
            basis: Basis::NoGrant,
        },
        |(_, grant)| Decision {
            answer: grant.answer,
            basis: Basis::Grant(grant),
        },
    )
}

/// One source of authority: the subjects someone is known by, the grants that
/// may name them, and the rooms they own.
#[derive(Clone, Debug, Default)]
pub struct Party<'g> {
    pub subjects: Vec<Subject>,
    pub grants: Vec<&'g Grant>,
    /// Nodes owned structurally: everything under them is allowed, whatever
    /// the grants say.
    pub owns: Vec<TreePath>,
}

impl<'g> Party<'g> {
    #[must_use]
    pub fn new(subjects: Vec<Subject>, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        Self {
            subjects,
            grants: grants.into_iter().collect(),
            owns: Vec::new(),
        }
    }

    #[must_use]
    pub fn owning(mut self, node: TreePath) -> Self {
        self.owns.push(node);
        self
    }

    /// Adds grants from another source (a room's grants and a delegation's own,
    /// for instance).
    #[must_use]
    pub fn with_grants(mut self, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        self.grants.extend(grants);
        self
    }

    #[must_use]
    pub fn decide(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Decision<'g> {
        if self.owns.iter().any(|owned| owned.covers(node)) {
            return Decision {
                answer: Answer::Allow,
                basis: Basis::Owner,
            };
        }
        decide(self.grants.iter().copied(), &self.subjects, node, kind, cx)
    }
}

/// Who is acting: a person, and the delegations bounding them (an agent, a
/// task, a Shortcut, an app, a foreign device).
#[derive(Clone, Debug, Default)]
pub struct Authority<'g> {
    pub person: Party<'g>,
    pub bounds: Vec<Party<'g>>,
}

impl<'g> Authority<'g> {
    #[must_use]
    pub const fn person(person: Party<'g>) -> Self {
        Self {
            person,
            bounds: Vec::new(),
        }
    }

    /// Bounds the authority by a delegation.
    #[must_use]
    pub fn bounded_by(mut self, bound: Party<'g>) -> Self {
        self.bounds.push(bound);
        self
    }

    /// `min(decide(person), decide(each delegation))`: a delegation never does
    /// more than the person, and the person's restrictions reach it.
    #[must_use]
    pub fn decide(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'g> {
        let mut parts = Vec::with_capacity(self.bounds.len().saturating_add(1));
        parts.push(self.person.decide(node, kind, cx));
        parts.extend(self.bounds.iter().map(|bound| bound.decide(node, kind, cx)));
        let answer = parts
            .iter()
            .map(|part| part.answer)
            .min()
            .unwrap_or(Answer::Deny);
        Verdict {
            answer,
            node: node.clone(),
            kind,
            parts,
        }
    }
}

/// The combined answer for one node and kind, with each party's part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict<'g> {
    pub answer: Answer,
    pub node: TreePath,
    pub kind: ActionKind,
    /// The person's decision first, then each delegation's in order.
    pub parts: Vec<Decision<'g>>,
}

impl<'g> Verdict<'g> {
    /// The `Once` grants this verdict relied on. A door that runs the action
    /// revokes them.
    pub fn once_grants(&self) -> impl Iterator<Item = &'g Grant> + '_ {
        self.parts
            .iter()
            .filter_map(Decision::grant)
            .filter(|grant| grant.lifetime == Lifetime::Once)
    }

    /// The decision that set the answer: the first part giving the minimum.
    #[must_use]
    pub fn deciding(&self) -> Option<&Decision<'g>> {
        self.parts.iter().find(|part| part.answer == self.answer)
    }

    /// Why, in a sentence people and agents can read.
    #[must_use]
    pub fn explain(&self) -> String {
        let outcome = match self.answer {
            Answer::Allow => "Allowed",
            Answer::Ask => "Asks first",
            Answer::Deny => "Not allowed",
        };
        let who = |index: usize| {
            if index == 0 {
                "the person's grants".to_owned()
            } else {
                "the delegation's grants".to_owned()
            }
        };
        let index = self
            .parts
            .iter()
            .position(|part| part.answer == self.answer)
            .unwrap_or(0);
        let reason = match self.parts.get(index).map(|part| part.basis) {
            Some(Basis::Owner) => "the person owns the room".to_owned(),
            Some(Basis::Grant(grant)) => format!(
                "{} give {} on {} the answer \"{}\" (grant {})",
                who(index),
                self.kind.phrase(),
                grant.node,
                grant.answer,
                grant.id
            ),
            Some(Basis::NoGrant) | None => {
                format!(
                    "{} do not cover {} on {}",
                    who(index),
                    self.kind.phrase(),
                    self.node
                )
            }
        };
        format!("{outcome}: {reason}.")
    }
}
