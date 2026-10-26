//! The one decision every door, host and receiver makes.

use crate::{ActionKind, Answer, Context, Grant, GrantId, Lifetime, Subject, TreePath};

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
    let mut best: Option<&'g Grant> = None;
    for grant in grants {
        if !grant.kinds.contains(kind)
            || !subjects.contains(&grant.subject)
            || !grant.node.covers(node)
            || !grant.is_live(cx)
        {
            continue;
        }
        let better = best.is_none_or(|held| match grant.node.cmp_specificity(&held.node) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => grant.answer < held.answer,
            std::cmp::Ordering::Less => false,
        });
        if better {
            best = Some(grant);
        }
    }
    best.map_or(
        Decision {
            answer: Answer::Deny,
            basis: Basis::NoGrant,
        },
        |grant| Decision {
            answer: grant.answer,
            basis: Basis::Grant(grant),
        },
    )
}

/// [`decide`] for one subject by itself: what the person holds and what their
/// delegation holds are each read this way before [`decide_delegated`]
/// combines them.
#[must_use]
pub fn decide_subject<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    subject: &Subject,
    node: &TreePath,
    kind: ActionKind,
    cx: &Context<'_>,
) -> Decision<'g> {
    decide(grants, std::slice::from_ref(subject), node, kind, cx)
}

/// Someone acting for a person: `min(decide(person), decide(delegation))`, with
/// Deny < Ask < Allow, each side decided over the grants naming its subject.
/// A delegation never does more than the person, and the person's restriction
/// always reaches it.
#[must_use]
pub fn decide_delegated<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    person: &Subject,
    delegation: &Subject,
    node: &TreePath,
    kind: ActionKind,
    cx: &Context<'_>,
) -> Decision<'g> {
    let grants: Vec<&Grant> = grants.into_iter().collect();
    let person = decide(
        grants.iter().copied(),
        std::slice::from_ref(person),
        node,
        kind,
        cx,
    );
    let delegation = decide(
        grants.iter().copied(),
        std::slice::from_ref(delegation),
        node,
        kind,
        cx,
    );
    if delegation.answer < person.answer {
        delegation
    } else {
        person
    }
}

/// Why a decision came out as it did, in a sentence people and agents can
/// read: the node, the kind and the grant that decided it, or that no grant
/// covers it.
#[must_use]
pub fn explain(decision: &Decision, node: &TreePath, kind: ActionKind) -> String {
    let outcome = match decision.answer {
        Answer::Allow => "Allowed",
        Answer::Ask => "Asks first",
        Answer::Deny => "Not allowed",
    };
    let reason = match decision.basis {
        Basis::Owner => "the room is theirs".to_owned(),
        Basis::Grant(grant) => format!(
            "a grant {} {} for {}",
            kind.phrase(),
            answer_phrase(grant.answer),
            grant.node,
        ),
        Basis::NoGrant => format!("no grant covers {} on {node}", kind.phrase()),
    };
    format!("{outcome}: {reason}.")
}

fn answer_phrase(answer: Answer) -> &'static str {
    match answer {
        Answer::Allow => "allows",
        Answer::Ask => "asks first for",
        Answer::Deny => "refuses",
    }
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
    /// Denies no more specific grant lifts: the room's ban or restriction
    /// of this person. A grant to their role (a share of one item with the
    /// room, a role's rule on comments) speaks for the role, not for a
    /// decision about them.
    pub held_off: Vec<&'g Grant>,
}

impl<'g> Party<'g> {
    #[must_use]
    pub fn new(subjects: Vec<Subject>, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        Self {
            subjects,
            grants: grants.into_iter().collect(),
            owns: Vec::new(),
            held_off: Vec::new(),
        }
    }

    #[must_use]
    pub fn owning(mut self, node: TreePath) -> Self {
        self.owns.push(node);
        self
    }

    /// The same party, with `denies` held over every more specific grant
    /// (see [`Self::held_off`]). Grants that are not denies hold nothing off.
    #[must_use]
    pub fn held_off_by(mut self, denies: impl IntoIterator<Item = &'g Grant>) -> Self {
        self.held_off.extend(
            denies
                .into_iter()
                .filter(|grant| grant.answer == Answer::Deny),
        );
        self
    }

    /// Adds grants from another source (a room's grants and a delegation's own,
    /// for instance).
    #[must_use]
    pub fn with_grants(mut self, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        self.grants.extend(grants);
        self
    }

    /// The same party, known as `subject` too: an agent's run of an issue
    /// is the agent and the issue, so grants naming either decide for it,
    /// the most specific first.
    #[must_use]
    pub fn also_as(mut self, subject: Subject) -> Self {
        self.subjects.push(subject);
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
        if let Some(deny) = self.held_off.iter().copied().find(|grant| {
            grant.kinds.contains(kind) && grant.node.covers(node) && grant.is_live(cx)
        }) {
            return Decision {
                answer: Answer::Deny,
                basis: Basis::Grant(deny),
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

impl Authority<'_> {
    /// [`Self::decide`]'s answer alone, made [`Verdict::privileged`] when
    /// `privileged`, without keeping each party's decision: what a check
    /// that only needs the answer asks.
    #[must_use]
    pub fn answer(
        &self,
        node: &TreePath,
        kind: ActionKind,
        privileged: bool,
        cx: &Context<'_>,
    ) -> Answer {
        let person = self.person.decide(node, kind, cx).answer;
        if privileged && person == Answer::Ask {
            return Answer::Deny;
        }
        self.bounds
            .iter()
            .map(|bound| bound.decide(node, kind, cx).answer)
            .fold(person, Answer::min)
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
    /// This verdict for a privileged action, one that exercises authority
    /// over the room or others in it: the person's own grants must allow it.
    /// Where they only ask, the person's confirmation is no authority they
    /// lack, so it is refused. A delegation's Ask still asks the person.
    #[must_use]
    pub fn privileged(mut self) -> Self {
        if self
            .parts
            .first()
            .is_some_and(|person| person.answer == Answer::Ask)
        {
            self.answer = Answer::Deny;
        }
        self
    }

    /// The `Once` grants this verdict relied on. A door that runs the action
    /// revokes them.
    pub fn once_grants(&self) -> impl Iterator<Item = &'g Grant> + '_ {
        self.parts
            .iter()
            .filter_map(Decision::grant)
            .filter(|grant| grant.lifetime == Lifetime::Once)
    }

    /// The `Once` grants a run on this verdict spends: those it relied on,
    /// unless it refuses. The door that runs the action hands them to
    /// [`crate::RoomPolicy::spend`] (or its host's revocation of them), so a
    /// grant given for one use is gone after it.
    #[must_use]
    pub fn spent(&self) -> Vec<GrantId> {
        if self.answer == Answer::Deny {
            return Vec::new();
        }
        self.once_grants().map(|grant| grant.id.clone()).collect()
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
