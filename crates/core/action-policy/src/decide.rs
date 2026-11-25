//! The one decision every action API, host and receiver makes.

use std::cmp::Ordering;

use tilo_app_id::AppId;

use crate::{
    ActionKind, Answer, Context, Grant, GrantId, Grantee, KindSet, Level, Lifetime, TreePath,
};

/// Why a party answered as it did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Basis<'g> {
    /// The most specific live grant covering the node and kind.
    Grant(&'g Grant),
    /// The cap the code puts on the party, below what its grants allow.
    Capped(Cap),
    /// The cap of a device that keeps no root of its person's, for an action only that root
    /// signs ([`Cap::root_only`]).
    RootOnly(Cap),
    /// The party owns the workspace the node is in.
    Owner,
    /// The node is the party's own: their view of the layout, a private record of theirs
    /// ([`crate::roles::own_records`]), or their own device's settings
    /// ([`crate::roles::is_device_settings`]).
    OwnRecord,
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
    /// Whether the call is answered at one of the person's devices that keeps their root: asked
    /// there once, and run there under that root ([`Cap::root_only`]).
    #[must_use]
    pub const fn answered_at_root(&self) -> bool {
        matches!(self.basis, Basis::RootOnly(_)) && matches!(self.answer, Answer::Ask)
    }

    #[must_use]
    pub const fn grant(&self) -> Option<&'g Grant> {
        match self.basis {
            Basis::Grant(grant) => Some(grant),
            _ => None,
        }
    }
}

/// Decides for one set of grantees over one set of grants:
///
/// 1. Take the live grants naming one of `grantees` ([`names`]), whose node is
///    `node` or an ancestor of it, and whose kinds include `kind`.
/// 2. The most specific node wins.
/// 3. For content at the same node, personal overrides come before custom
///    roles, then the preset.
/// 4. Custom roles combine by taking the most permissive answer. Among
///    competing personal or preset rules the strictest answer wins.
///    Governance uses the strictest answer at each scope.
/// 5. No grant: Deny.
///
/// A node in a person's view of the layout is decided as one
/// ([`crate::roles::decided_node`]). A device's settings are its own person's
/// ([`crate::roles::is_device_settings`]): the workspace's ownership and its roles do not reach
/// another person's device, and only a grant naming the person does, wherever it was decided.
pub fn decide<'g>(
    grants: impl IntoIterator<Item = &'g Grant>,
    grantees: &[Grantee],
    node: &TreePath,
    kind: ActionKind,
    cx: &Context<'_>,
) -> Decision<'g> {
    let node = &*crate::roles::decided_node(node);
    let governs = node.id_at(Level::Kind) == Some(crate::roles::WORKSPACE_KIND);
    let mut best: Option<&'g Grant> = None;
    for grant in grants {
        if !grant.kinds.contains(kind)
            || !names(grant, grantees, cx)
            || !cx.covers(&grant.node, node)
            || !grant.is_live(cx)
        {
            continue;
        }
        let better = best.is_none_or(|held| {
            grant.node.cmp_specificity(&held.node).then_with(|| {
                if governs {
                    held.answer.cmp(&grant.answer)
                } else {
                    compare_content_rules(grant, held)
                }
            }) == Ordering::Greater
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

fn compare_content_rules(candidate: &Grant, current: &Grant) -> Ordering {
    let priority = |rule: &Grant| {
        if rule.grantee.person_id().is_some() {
            2
        } else {
            u8::from(matches!(rule.grantee, Grantee::CustomRole(_)))
        }
    };
    priority(candidate).cmp(&priority(current)).then_with(|| {
        if matches!(candidate.grantee, Grantee::CustomRole(_)) {
            candidate.answer.cmp(&current.answer)
        } else {
            current.answer.cmp(&candidate.answer)
        }
    })
}

/// Whether `grant` names one of `grantees`. A grant to a role names everyone
/// holding that role or one that holds all of its grants ([`crate::Role::includes`]),
/// except a built-in role's own grants: those are the role's rules
/// ([`crate::roles::role_rules`]), each the whole answer for its role, and a
/// role holding them carries them already.
#[must_use]
pub fn names(grant: &Grant, grantees: &[Grantee], cx: &Context<'_>) -> bool {
    if cx.grantee_matches(&grant.grantee, grantees) {
        return true;
    }
    if crate::roles::is_role_grant(grant) {
        return false;
    }
    grant.grantee.role_of().is_some_and(|wanted| {
        grantees
            .iter()
            .any(|held| held.role_of().is_some_and(|held| held.includes(wanted)))
    })
}

/// One source of authority: the grantees someone is known by, the grants that
/// may name them, and the workspaces they own.
#[derive(Clone, Debug, Default)]
pub struct Party<'g> {
    pub grantees: Vec<Grantee>,
    pub grants: Vec<&'g Grant>,
    /// Nodes owned structurally: everything under them is allowed, whatever
    /// the grants say.
    pub owns: Vec<TreePath>,
    /// What is the party's own, and what it may do there whatever its role's grants say
    /// ([`crate::roles::own_records`]).
    pub own_records: Vec<(TreePath, KindSet)>,
    /// Denies no more specific grant lifts: the workspace's ban or restriction
    /// of this person. A grant to their role (a share of one item with the
    /// workspace, a role's rule on comments) speaks for the role, not for a
    /// decision about them.
    pub held_off: Vec<&'g Grant>,
    /// A cap the code puts on this party, below whatever its grants allow.
    pub cap: Option<Cap>,
    /// Grants a delegation hands the party (an agent's preset, its certificate's), which decide
    /// for it only while none of `narrowing` stands ([`Self::narrowed_by`]).
    pub delegated: Vec<&'g Grant>,
    /// The answers that narrow the party to what they name: while one stands, `delegated`
    /// decides nothing.
    pub narrowing: Vec<&'g Grant>,
}

/// What a device or a piece of software may do at most, whatever its grants
/// say: the code's rule about the thing acting, not a grant anyone gave.
///
/// A capped call is not a refusal. What only the person's root signs, an action
/// declares (`tilo_actions::ROOT_ONLY_TAG`), and the cap of a device that keeps
/// no root answers it [`Answer::Ask`], answered at a root-holding device
/// ([`Decision::answered_at_root`]). The capped device keeps the call as its
/// waiting invocation record in the workspace, with no special relay. A
/// root-holding device takes the record up when it comes online, asks the
/// person once through the one ask whatever the action's own confirmation
/// says, makes the change under its own key and writes the outcome into the
/// record. The capped device follows the record and shows the outcome. While no
/// root-holding device is online, the record stays asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cap {
    /// What it may do to the workspace's content.
    pub content: Answer,
    /// What it may do outside the workspace.
    pub outside: Answer,
    /// What it may do to the workspace's own resource (its governance).
    pub governance: Answer,
    /// What it may do that only the person's root signs: [`Answer::Ask`] is answered at a
    /// device that keeps that root ([`Decision::answered_at_root`]).
    pub root_only: Answer,
    /// Why, in words for the person it holds back.
    pub why: &'static str,
}

impl Cap {
    /// A device that is not the person's: it reads and changes content, asks
    /// before reaching outside, and never governs.
    pub const FOREIGN_DEVICE: Self = Self {
        content: Answer::Allow,
        outside: Answer::Ask,
        governance: Answer::Deny,
        root_only: Answer::Deny,
        why: "this computer is not one of the person's own Tilo devices, so from it they \
              change content, are asked before reaching outside the workspace, and leave its \
              members and settings to a device of their own",
    };

    /// One of the person's own devices their devices record does not mark as keeping their root:
    /// it does all its grants allow, and what only the root signs is answered at one of their
    /// devices that keeps it.
    pub const ROOTLESS_DEVICE: Self = Self {
        content: Answer::Allow,
        outside: Answer::Allow,
        governance: Answer::Allow,
        root_only: Answer::Ask,
        why: "only a device of the person's that keeps their identity signs this, so it is \
              asked and carried out there",
    };

    /// What this cap answers an action only the person's root signs (`tilo_actions::ROOT_ONLY_TAG`).
    #[must_use]
    pub const fn root_only(self) -> Decision<'static> {
        Decision {
            answer: self.root_only,
            basis: Basis::RootOnly(self),
        }
    }

    /// What this cap answers for `kind` at `node` in the workspace it governs.
    fn answer(self, node: &TreePath, kind: ActionKind) -> Answer {
        if node.id_at(Level::Kind) == Some(crate::roles::WORKSPACE_KIND) {
            return match kind {
                ActionKind::Read => Answer::Allow,
                ActionKind::Undoable | ActionKind::Outside => self.governance,
            };
        }
        match kind {
            ActionKind::Read | ActionKind::Undoable => self.content,
            ActionKind::Outside => self.outside,
        }
    }
}

impl<'g> Party<'g> {
    #[must_use]
    pub fn new(grantees: Vec<Grantee>, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        Self {
            grantees,
            grants: grants.into_iter().collect(),
            owns: Vec::new(),
            own_records: Vec::new(),
            held_off: Vec::new(),
            cap: None,
            delegated: Vec::new(),
            narrowing: Vec::new(),
        }
    }

    /// The same party, capped by `cap`.
    #[must_use]
    pub const fn capped(mut self, cap: Cap) -> Self {
        self.cap = Some(cap);
        self
    }

    #[must_use]
    pub fn owning(mut self, node: TreePath) -> Self {
        self.owns.push(node);
        self
    }

    /// The same party, with `records` its own ([`crate::roles::own_records`]).
    #[must_use]
    pub fn owning_records(mut self, records: Vec<(TreePath, KindSet)>) -> Self {
        self.own_records.extend(records);
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

    /// Adds grants from another source (a workspace's grants and a delegation's own,
    /// for instance).
    #[must_use]
    pub fn with_grants(mut self, grants: impl IntoIterator<Item = &'g Grant>) -> Self {
        self.grants.extend(grants);
        self
    }

    /// The same party, handed `grants` by a delegation, which decide for it only while none of
    /// `answers` stands at the moment it decides: an agent's person who answered it narrows it to
    /// what those answers name, wherever its other grants came from.
    #[must_use]
    pub fn narrowed_by(
        mut self,
        grants: impl IntoIterator<Item = &'g Grant>,
        answers: impl IntoIterator<Item = &'g Grant>,
    ) -> Self {
        self.delegated.extend(grants);
        self.narrowing.extend(answers);
        self
    }

    /// The same party, known as `grantee` too: an agent's run of an issue
    /// is the agent and the issue, so grants naming either decide for it,
    /// the most specific first.
    #[must_use]
    pub fn also_as(mut self, grantee: Grantee) -> Self {
        self.grantees.push(grantee);
        self
    }

    #[must_use]
    pub fn decide(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Decision<'g> {
        let held = self.decide_grants(node, kind, cx);
        match self.cap {
            Some(cap) => {
                let answer = cap.answer(node, kind);
                if answer < held.answer {
                    Decision {
                        answer,
                        basis: Basis::Capped(cap),
                    }
                } else {
                    held
                }
            }
            None => held,
        }
    }

    fn decide_grants(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Decision<'g> {
        let node = &*crate::roles::decided_node(node);
        if kind == ActionKind::Read && crate::roles::lacks_private_record_owner_at(node) {
            return Decision {
                answer: Answer::Deny,
                basis: Basis::NoGrant,
            };
        }
        let device_settings = crate::roles::is_device_settings(node);
        // A person's private record is theirs, not the workspace's owner's: only its person
        // ([`crate::roles::own_records`]) or a grant reaches it. Nor does the workspace's
        // ownership reach another person's device ([`crate::roles::is_device_settings`]).
        if !device_settings
            && !crate::roles::in_private_record(node)
            && self.owns.iter().any(|owned| cx.covers(owned, node))
        {
            return Decision {
                answer: Answer::Allow,
                basis: Basis::Owner,
            };
        }
        if let Some(deny) = self.held_off.iter().copied().find(|grant| {
            grant.kinds.contains(kind) && cx.covers(&grant.node, node) && grant.is_live(cx)
        }) {
            return Decision {
                answer: Answer::Deny,
                basis: Basis::Grant(deny),
            };
        }
        if device_settings {
            // A device's settings are its own person's, in their own workspace. Elsewhere only a
            // grant naming a member reaches them: a role preset reaches another person's device
            // only where that person granted it there.
            if self.owns_device_settings(node) {
                return Decision {
                    answer: Answer::Allow,
                    basis: Basis::OwnRecord,
                };
            }
            let shared = self
                .deciding(cx)
                .filter(|grant| grant.grantee.as_member().is_some());
            return decide(shared, &self.grantees, node, kind, cx);
        }
        if self
            .own_records
            .iter()
            .any(|(own, kinds)| kinds.contains(kind) && cx.covers(own, node))
        {
            return Decision {
                answer: Answer::Allow,
                basis: Basis::OwnRecord,
            };
        }
        decide(self.deciding(cx), &self.grantees, node, kind, cx)
    }

    /// Whether a device's settings `node` are this party's own: its person is the one whose own
    /// workspace keeps them ([`crate::roles::person_workspace_id`]). Another person's device is
    /// nobody's here but that person's.
    fn owns_device_settings(&self, node: &TreePath) -> bool {
        let Some(workspace) = node.id_at(Level::Workspace) else {
            return false;
        };
        self.grantees
            .iter()
            .filter_map(Grantee::person_id)
            .any(|person| {
                crate::roles::workspace_of_person(person).is_some_and(|own| own == workspace)
            })
    }

    /// The grants that can decide for this party: those naming one of its grantees
    /// ([`names`]). Every decision it makes reads these and no other.
    fn deciding<'s>(&'s self, cx: &'s Context<'_>) -> impl Iterator<Item = &'g Grant> + 's {
        let narrowed = self.narrowing.iter().any(|answer| answer.is_live(cx));
        let delegated: &[&'g Grant] = if narrowed { &[] } else { &self.delegated };
        self.grants
            .iter()
            .chain(delegated)
            .copied()
            .filter(move |grant| names(grant, &self.grantees, cx))
    }

    /// What this party decides from, as owned data ([`Effective`]).
    fn effective(&self) -> EffectiveParty {
        // Which grants name a grantee does not depend on when or where it is decided.
        let cx = Context::at(0);
        EffectiveParty {
            grantees: self.grantees.clone(),
            grants: self.deciding(&cx).cloned().collect(),
            owns: self.owns.clone(),
            own_records: self.own_records.clone(),
            held_off: self.held_off.iter().copied().cloned().collect(),
            cap: self.cap,
            delegated: self.delegated.iter().copied().cloned().collect(),
            narrowing: self.narrowing.iter().copied().cloned().collect(),
        }
    }
}

/// Everything an [`Authority`] decides from, as owned data: two authorities with
/// equal ones answer every action alike at any one time, node and context. What
/// tells them apart is only what changes a decision: a role or custom role the
/// person holds, a grant naming them or a delegation bounding them, a ban or
/// restriction of them, owning the workspace, a cap, the app and what it
/// declares. Another member's admission, role or grants change none of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    parties: Vec<EffectiveParty>,
    people: usize,
    app: Option<AppId>,
    app_kinds: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct EffectiveParty {
    grantees: Vec<Grantee>,
    grants: Vec<Grant>,
    owns: Vec<TreePath>,
    own_records: Vec<(TreePath, KindSet)>,
    held_off: Vec<Grant>,
    cap: Option<Cap>,
    delegated: Vec<Grant>,
    narrowing: Vec<Grant>,
}

impl Effective {
    /// Whether a grant it decides by stops being live after `from_ms` and by
    /// `to_ms` (Unix milliseconds): the one way time alone changes its answers.
    #[must_use]
    pub fn lapses_between(&self, from_ms: u64, to_ms: u64) -> bool {
        self.parties
            .iter()
            .flat_map(|party| {
                party
                    .grants
                    .iter()
                    .chain(&party.held_off)
                    .chain(&party.delegated)
                    .chain(&party.narrowing)
            })
            .any(|grant| match grant.lifetime {
                Lifetime::Until(at) => from_ms < at && at <= to_ms,
                _ => false,
            })
    }
}

/// Who is acting: a person, and the delegations bounding them (an agent, a
/// task, a Shortcut, a foreign device).
#[derive(Clone, Debug, Default)]
pub struct Authority<'g> {
    pub person: Party<'g>,
    pub bounds: Vec<Party<'g>>,
    /// How many of `person` and `bounds`, in order, are people of the acting: a
    /// privileged action needs each of them to allow it.
    pub people: usize,
    /// The app the change is made through.
    pub app: Option<&'g AppId>,
    /// The resource kinds the app declares: in its own node, its records and these kinds alone.
    /// `None` where the host deciding does not hold the app's manifest.
    pub app_kinds: Option<&'g [String]>,
}

impl<'g> Authority<'g> {
    #[must_use]
    pub const fn person(person: Party<'g>) -> Self {
        Self {
            person,
            bounds: Vec::new(),
            people: 1,
            app: None,
            app_kinds: None,
        }
    }

    /// The same authority, with `kinds` as what the app it acts through may
    /// reach in its own node.
    #[must_use]
    pub fn in_app(mut self, kinds: &'g [String]) -> Self {
        self.app_kinds = Some(kinds);
        self
    }

    /// The same authority, bounded by `party` too.
    #[must_use]
    pub fn also_as(mut self, party: Party<'g>) -> Self {
        self.bounds.push(party);
        self
    }

    #[must_use]
    pub fn bounded_by(mut self, bound: Party<'g>) -> Self {
        self.bounds.push(bound);
        self
    }

    /// What this authority decides from ([`Effective`]): compare two to know
    /// whether anything they may do differs.
    #[must_use]
    pub fn effective(&self) -> Effective {
        Effective {
            parties: std::iter::once(&self.person)
                .chain(&self.bounds)
                .map(Party::effective)
                .collect(),
            people: self.people,
            app: self.app.cloned(),
            app_kinds: self.app_kinds.map(<[String]>::to_vec),
        }
    }

    /// `min(decide(person), decide(each delegation))`: a delegation never does
    /// more than the person, and the person's restrictions reach it. Where the
    /// person's part would ask them, and the one delegation, their agent, holds
    /// the answer they gave it for exactly this action, that answer is theirs,
    /// given before the action came ([`Self::answered_in_advance`]).
    #[must_use]
    pub fn decide(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'g> {
        let mut parts = Vec::with_capacity(self.bounds.len().saturating_add(1));
        parts.push(self.person.decide(node, kind, cx));
        parts.extend(self.bounds.iter().map(|bound| bound.decide(node, kind, cx)));
        let mut answer = if self.answers_in_advance(&parts) {
            Answer::Allow
        } else {
            parts
                .iter()
                .map(|part| part.answer)
                .min()
                .unwrap_or(Answer::Deny)
        };
        if self.unowned_actor_reads_own_record(node, kind) {
            answer = Answer::Deny;
        }
        if let (Some(app), Some(kinds)) = (self.app, self.app_kinds)
            && node.id_at(Level::App) == Some(app.as_str())
            && match node.id_at(Level::Kind) {
                Some(of) => !kinds.iter().any(|declared| declared == of),
                None => node.id_at(Level::Resource).is_none(),
            }
        {
            answer = Answer::Deny;
        }
        Verdict {
            answer,
            node: node.clone(),
            kind,
            parts,
            people: self.people,
        }
    }
}

impl Authority<'_> {
    /// Whether this authority may read or change `node` or anything within it:
    /// `node` itself, or a node below it that one of its grants names, as a
    /// bot's program names the one action of a record it runs. What a
    /// document's keys seal is the whole document, so whoever may act on any
    /// part of it needs them.
    #[must_use]
    pub fn acts_within(&self, node: &TreePath, cx: &Context<'_>) -> bool {
        let allows = |at: &TreePath| {
            [ActionKind::Read, ActionKind::Undoable]
                .into_iter()
                .any(|kind| self.decide(at, kind, cx).answer == Answer::Allow)
        };
        allows(node)
            || std::iter::once(&self.person)
                .chain(&self.bounds)
                .flat_map(|party| party.deciding(cx))
                .filter(|grant| grant.answer == Answer::Allow && grant.is_live(cx))
                .filter_map(|grant| node.toward(&grant.node))
                .any(|at| allows(&at))
    }

    /// Whether no person acts here and `node` is in a person's private record, which only
    /// its person reads.
    fn unowned_actor_reads_own_record(&self, node: &TreePath, kind: ActionKind) -> bool {
        kind == ActionKind::Read
            && self.people == 0
            && crate::roles::in_private_record(&crate::roles::decided_node(node))
    }

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
        if self.unowned_actor_reads_own_record(node, kind) {
            return Answer::Deny;
        }
        let person = self.person.decide(node, kind, cx).answer;
        if privileged && self.people <= 1 && person == Answer::Ask {
            return Answer::Deny;
        }
        if privileged
            && self
                .bounds
                .iter()
                .take(self.people.saturating_sub(1))
                .any(|bound| bound.decide(node, kind, cx).answer == Answer::Ask)
        {
            return Answer::Deny;
        }
        if person == Answer::Ask
            && let [bound] = self.bounds.as_slice()
        {
            let parts = [
                Decision {
                    answer: person,
                    basis: Basis::NoGrant,
                },
                bound.decide(node, kind, cx),
            ];
            if self.answers_in_advance(&parts) {
                return Answer::Allow;
            }
        }
        self.bounds
            .iter()
            .map(|bound| bound.decide(node, kind, cx).answer)
            .fold(person, Answer::min)
    }

    /// Whether `verdict` allows the call by the answer the person gave their agent in advance
    /// for this very action or resource ([`crate::agent_answers::AnswerScope::Action`], what
    /// "Always allow this" gives): a call such an answer allows runs unasked, though it reaches
    /// outside Tilo or its person attends, because asking would put to them what they already
    /// answered. A broader answer (a whole app or kind of resource) lifts no such ask.
    #[must_use]
    pub fn answered_in_advance(&self, verdict: &Verdict<'_>) -> bool {
        verdict.parts.iter().skip(self.people.max(1)).any(|part| {
            part.answer == Answer::Allow
                && matches!(part.basis, Basis::Grant(grant)
                if set_by_its_person(grant)
                    && self.person.grantees.contains(&grant.granted_by)
                    && matches!(
                        crate::agent_answers::AnswerScope::of_node(&grant.node),
                        Some(
                            crate::agent_answers::AnswerScope::Action { .. }
                                | crate::agent_answers::AnswerScope::Resource { .. }
                        )
                    ))
        })
    }

    /// Whether `parts` (the person's decision, then each delegation's) answer an
    /// action the person would be asked about with the answer they gave in
    /// advance: one person, one delegation (their agent), the person's part
    /// asks, and the agent's allows by the answer that same person gave it for
    /// exactly this action ([`crate::agent_answers::AnswerScope::Action`], what
    /// "Always allow this" and the agent's settings give). Asking them would
    /// put to them what they already answered. What the person may not do at
    /// all stays refused, and no answer for a whole app or kind of resource
    /// stands in for them.
    fn answers_in_advance(&self, parts: &[Decision<'_>]) -> bool {
        let [person, agent] = parts else {
            return false;
        };
        self.people == 1
            && person.answer == Answer::Ask
            && agent.answer == Answer::Allow
            && matches!(agent.basis, Basis::Grant(grant)
            if set_by_its_person(grant)
                && self.person.grantees.contains(&grant.granted_by)
                && matches!(
                    crate::agent_answers::AnswerScope::of_node(&grant.node),
                    Some(
                        crate::agent_answers::AnswerScope::Action { .. }
                            | crate::agent_answers::AnswerScope::Resource { .. }
                    )
                ))
    }
}

/// Whether `grant` is an answer an agent's person gave it
/// ([`crate::agent_answers`]).
fn set_by_its_person(grant: &Grant) -> bool {
    crate::agent_answers::is_answer(grant)
}

/// What keeps the acting from an action it is refused ([`Verdict::refused_by`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refuser {
    /// The answers the agent's person gave it in the workspace's settings, under Agents.
    Settings,
    /// The workspace's own rules: its roles and grants for the person, a ban, or what software
    /// may do there at most.
    Workspace,
}

/// The combined answer for one node and kind, with each party's part.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict<'g> {
    pub answer: Answer,
    pub node: TreePath,
    pub kind: ActionKind,
    /// The person's decision first, then each delegation's in order.
    pub parts: Vec<Decision<'g>>,
    /// How many of `parts`, in order, are the acting's people.
    pub people: usize,
}

impl<'g> Verdict<'g> {
    /// This verdict for a privileged action, one that exercises authority
    /// over the workspace or others in it: every person's own grants must allow
    /// it. Where one of them only asks, their confirmation is no authority they
    /// lack, so it is refused. A delegation's Ask still asks the person.
    #[must_use]
    pub fn privileged(mut self) -> Self {
        if self
            .parts
            .iter()
            .take(self.people.max(1))
            .any(|person| person.answer == Answer::Ask)
        {
            self.answer = Answer::Deny;
        }
        self
    }

    /// The `Once` grants this verdict relied on. An action API that runs the action
    /// revokes them.
    pub fn once_grants(&self) -> impl Iterator<Item = &'g Grant> + '_ {
        self.parts
            .iter()
            .filter_map(Decision::grant)
            .filter(|grant| grant.lifetime == Lifetime::Once)
    }

    /// The `Once` grants a run on this verdict spends: those it relied on,
    /// unless it refuses. The API that runs the action hands them to
    /// [`crate::WorkspacePolicy::spend`] (or its host's revocation of them), so a
    /// grant given for one use is gone after it.
    #[must_use]
    pub fn spent(&self) -> Vec<GrantId> {
        if self.answer == Answer::Deny {
            return Vec::new();
        }
        self.once_grants().map(|grant| grant.id.clone()).collect()
    }

    /// What keeps the acting from an action this verdict refuses: the answers its person gave
    /// the agent ([`Refuser::Settings`]) when those alone deny it, else the workspace's own rules
    /// ([`Refuser::Workspace`]). `None` when it does not refuse.
    #[must_use]
    pub fn refused_by(&self) -> Option<Refuser> {
        if self.answer != Answer::Deny {
            return None;
        }
        let denies = |part: &&Decision<'_>| part.answer == Answer::Deny;
        if self
            .parts
            .iter()
            .take(self.people)
            .any(|part| denies(&part))
        {
            return Some(Refuser::Workspace);
        }
        let mut delegations = self
            .parts
            .iter()
            .skip(self.people)
            .filter(denies)
            .peekable();
        let settings = delegations.peek().is_some()
            && delegations.all(|part| part.grant().is_some_and(set_by_its_person));
        Some(if settings {
            Refuser::Settings
        } else {
            Refuser::Workspace
        })
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
            if index < self.people {
                "the person's grants".to_owned()
            } else {
                "the grants of those acting for them".to_owned()
            }
        };
        let index = self
            .parts
            .iter()
            .position(|part| part.answer == self.answer)
            .unwrap_or(0);
        let reason = match self.parts.get(index).map(|part| part.basis) {
            Some(Basis::Owner) => "the person owns the workspace".to_owned(),
            Some(Basis::OwnRecord) => format!("{} is theirs alone", self.node),
            Some(Basis::Capped(cap) | Basis::RootOnly(cap)) => cap.why.to_owned(),
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
