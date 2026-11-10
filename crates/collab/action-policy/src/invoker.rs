//! Who invokes an action, and the one admission of an invocation.
//!
//! Every host that runs an action decides it here, whichever route it took:
//! the desktop's broker over its app sockets, the host running a hosted
//! workspace's records in confined workers, a page over the apps it runs.
//! An [`Invoker`] is who the action is decided for apart from the workspace's
//! policy: a person, held to the bounds of whoever acts for them (an agent,
//! a bot, a device that is not theirs), or a subject acting for nobody (an
//! application under its own identity). [`Invoker::authority`] is what they
//! may do under a policy, and [`admit_invocation`] weighs one invocation by
//! it: whether it runs, asks first, or is refused, and the `Once` grants
//! running it spends.

use crate::{
    ActionKind, Admission, Answer, Asker, Authority, Bound, Context, Grant, GrantId, Invoked,
    Party, Reach, Refused, Standing, Subject, TreePath, WorkspacePolicy, admit, roles,
};

/// Who an action is decided for, apart from the workspace whose policy
/// decides it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Invoker {
    /// A person, bounded by each of `bounds`: a device that is not theirs,
    /// and whoever acts for them. With no bounds, the person themselves.
    Person { person: String, bounds: Vec<Bound> },
    /// A subject acting for nobody (an application under its own identity,
    /// an agent no person delegated): only what the workspace grants it.
    Own(Subject),
}

impl Invoker {
    /// A person acting themselves.
    #[must_use]
    pub fn person(person: impl Into<String>) -> Self {
        Self::Person {
            person: person.into(),
            bounds: Vec::new(),
        }
    }

    /// `delegate` acting for `person`, carrying `carried` grants of its own
    /// besides those the workspace gives it: never more than the person, never
    /// more than its own grants allow.
    #[must_use]
    pub fn delegated(delegate: Subject, person: impl Into<String>, carried: Vec<Grant>) -> Self {
        Self::person(person).bounded_by(Bound {
            subject: delegate,
            grants: carried,
            also: Vec::new(),
        })
    }

    /// Agent `agent`, which `person`'s own device handed its session to in
    /// `workspace`: it carries the delegation a device gives its agents
    /// ([`roles::agent_turn`]: reading and undoable actions, reaching outside
    /// asking first, no governance).
    #[must_use]
    pub fn session_agent(workspace: &str, agent: &str, person: impl Into<String>) -> Self {
        let person = person.into();
        let bound = roles::agent_turn(workspace, agent, &person, &[]);
        Self::person(person).bounded_by(bound)
    }

    /// `subject` acting for nobody.
    #[must_use]
    pub const fn own(subject: Subject) -> Self {
        Self::Own(subject)
    }

    /// The same invoker, bounded by `bound` too. A subject acting for nobody
    /// has no person to bound, and stays as it is.
    #[must_use]
    pub fn bounded_by(mut self, bound: Bound) -> Self {
        if let Self::Person { bounds, .. } = &mut self {
            bounds.push(bound);
        }
        self
    }

    /// The same person, working from a device that is not theirs, when
    /// `foreign`.
    #[must_use]
    pub fn on_foreign_device(self, foreign: bool) -> Self {
        if !foreign {
            return self;
        }
        self.bounded_by(Bound {
            subject: Subject::ForeignDevice,
            grants: roles::foreign_device_bound(),
            also: Vec::new(),
        })
    }

    /// The same invoker, its delegate working in the run of the issue at
    /// `issue`: grants naming the issue decide for the run as the
    /// delegate's own do.
    #[must_use]
    pub fn working_on(mut self, issue: impl Into<String>) -> Self {
        if let Self::Person { bounds, .. } = &mut self
            && let Some(bound) = bounds
                .iter_mut()
                .find(|bound| bound.subject != Subject::ForeignDevice)
        {
            bound.also.push(Subject::Task(issue.into()));
        }
        self
    }

    /// The person the action is for, when it is for one.
    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        match self {
            Self::Person { person, .. } => Some(person),
            Self::Own(_) => None,
        }
    }

    /// Whoever acts for the person (an agent, a bot, an app they started),
    /// or the subject acting for nobody: `None` for a person acting
    /// themselves, from any device.
    #[must_use]
    pub fn delegate(&self) -> Option<&Subject> {
        match self {
            Self::Person { bounds, .. } => bounds
                .iter()
                .map(|bound| &bound.subject)
                .find(|subject| **subject != Subject::ForeignDevice),
            Self::Own(subject) => Some(subject),
        }
    }

    /// What this invoker may do under `policy`: the person's own authority,
    /// bounded by each of their bounds; or, acting for nobody, what the
    /// workspace grants the subject.
    #[must_use]
    pub fn authority<'a>(&'a self, policy: &'a WorkspacePolicy) -> Authority<'a> {
        match self {
            Self::Person { person, bounds } => authority_of(policy, person, bounds),
            Self::Own(subject) => Authority::person(policy.party_of(subject.clone())),
        }
    }

    /// This invoker's standing under `policy`, as a door or an app holds it.
    /// `None` for a subject acting for nobody, which no person's standing
    /// describes.
    #[must_use]
    pub fn standing(self, policy: WorkspacePolicy) -> Option<Standing> {
        match self {
            Self::Person { person, bounds } => Some(Standing {
                person,
                policy,
                bounds,
            }),
            Self::Own(_) => None,
        }
    }

    /// Who an invocation of this invoker's asks: the person, who can answer
    /// when they act themselves or when the host running it reaches them
    /// (`host_reaches_person`); and whether someone acts for them, who is
    /// asked before an action the person attends.
    #[must_use]
    pub fn asker(&self, host_reaches_person: bool) -> Asker {
        asker_of(self.delegate().is_some(), host_reaches_person)
    }
}

impl Standing {
    /// This person's standing, as a host read it from the workspace's
    /// policy (a device not theirs, a delegate device's own bound), held to
    /// `invoker`'s bounds too: the agent their device handed its session to,
    /// say. A bound the standing holds already is not added twice. `None`
    /// when `invoker` is not for this person.
    #[must_use]
    pub fn bounded_as(mut self, invoker: &Invoker) -> Option<Self> {
        let Invoker::Person { person, bounds } = invoker else {
            return None;
        };
        if *person != self.person {
            return None;
        }
        for bound in bounds {
            if !self.bounds.contains(bound) {
                self.bounds.push(bound.clone());
            }
        }
        Some(self)
    }

    /// Whoever acts for the person in this standing (an agent, a bot, an app
    /// they started): `None` for the person acting themselves, from any
    /// device.
    #[must_use]
    pub fn delegate(&self) -> Option<&Subject> {
        self.bounds
            .iter()
            .map(|bound| &bound.subject)
            .find(|subject| **subject != Subject::ForeignDevice)
    }

    /// Who an invocation in this standing asks ([`Invoker::asker`]).
    #[must_use]
    pub fn asker(&self, host_reaches_person: bool) -> Asker {
        asker_of(self.delegate().is_some(), host_reaches_person)
    }
}

/// [`Invoker::asker`] for an invoker acting through a delegate when
/// `delegated`.
#[must_use]
pub const fn asker_of(delegated: bool, host_reaches_person: bool) -> Asker {
    Asker {
        reachable: !delegated || host_reaches_person,
        delegated,
    }
}

/// `person`'s authority under `policy`, bounded by each of `bounds`.
pub(crate) fn authority_of<'a>(
    policy: &'a WorkspacePolicy,
    person: &str,
    bounds: &'a [Bound],
) -> Authority<'a> {
    bounds.iter().fold(
        Authority::person(policy.party(person)),
        |authority, bound| authority.bounded_by(bound.party(&policy.grants)),
    )
}

impl Bound {
    /// The party this delegation stands for in a workspace whose own grants
    /// are `workspace_grants`: its own grants and the workspace's grants
    /// naming its subject, or anyone it is known as too.
    #[must_use]
    pub fn party<'a>(&'a self, workspace_grants: &'a [Grant]) -> Party<'a> {
        let mut subjects = Vec::with_capacity(self.also.len().saturating_add(1));
        subjects.push(self.subject.clone());
        subjects.extend(self.also.iter().cloned());
        Party::new(subjects, workspace_grants).with_grants(&self.grants)
    }
}

/// One invocation, as [`admit_invocation`] weighs it.
#[derive(Clone, Copy, Debug)]
pub struct Invocation<'a> {
    /// The action's node.
    pub node: &'a TreePath,
    /// The kind of action it is.
    pub kind: ActionKind,
    /// Whether it exercises authority over the workspace or others in it
    /// ([`crate::Verdict::privileged`]).
    pub privileged: bool,
    /// Whether it needs its person attending.
    pub attended: bool,
    /// The node of the record it changes, decided as well where it is one:
    /// so a person restricted on one record cannot change it through an
    /// action they may otherwise take.
    pub record: Option<&'a TreePath>,
    /// How far the invocation it continues was admitted to reach, when it
    /// continues one: what goes further asks first.
    pub ceiling: Option<Reach>,
}

/// The grants' answer for an action of `kind`, privileged when
/// `privileged`, at `node`, for `authority` at `cx`, and the `Once` grants
/// relying on it spends. A step going further than `ceiling` (the
/// invocation it continues) is asked for as its own request.
#[must_use]
pub fn answer_at(
    authority: &Authority<'_>,
    (node, kind, privileged): (&TreePath, ActionKind, bool),
    ceiling: Option<Reach>,
    cx: &Context<'_>,
) -> (Answer, Vec<GrantId>) {
    let mut verdict = authority.decide(node, kind, cx);
    if privileged {
        verdict = verdict.privileged();
    }
    let exceeds = ceiling.is_some_and(|ceiling| !ceiling.covers(kind, privileged));
    let answer = match verdict.answer {
        Answer::Allow if exceeds => Answer::Ask,
        answer => answer,
    };
    (answer, verdict.spent())
}

/// Admit `invocation` for `authority`, asked of `asker`, at `cx`: whether
/// it runs or asks first, and the `Once` grants admitting it spends.
///
/// # Errors
/// Whose answer refused it.
pub fn admit_invocation(
    authority: &Authority<'_>,
    asker: Asker,
    invocation: Invocation<'_>,
    cx: &Context<'_>,
) -> Result<(Admission, Vec<GrantId>), Refused> {
    let (answer, spent) = answer_at(
        authority,
        (invocation.node, invocation.kind, invocation.privileged),
        invocation.ceiling,
        cx,
    );
    let record = invocation.record.map(|record| {
        answer_at(
            authority,
            (record, invocation.kind, false),
            invocation.ceiling,
            cx,
        )
        .0
    });
    let admission = admit(
        Invoked {
            answer,
            attended: invocation.attended,
            record,
        },
        asker,
    )?;
    Ok((admission, spent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KindSet, Role};

    const WORKSPACE: &str = "w";

    fn policy() -> WorkspacePolicy {
        let mut policy = WorkspacePolicy::new(WORKSPACE, "owner");
        policy.members.insert("editor".into(), Role::Editor);
        policy
    }

    fn invocation(node: &TreePath, kind: ActionKind) -> Invocation<'_> {
        Invocation {
            node,
            kind,
            privileged: false,
            attended: false,
            record: None,
            ceiling: None,
        }
    }

    fn admitted(
        invoker: &Invoker,
        policy: &WorkspacePolicy,
        invocation: Invocation<'_>,
    ) -> Result<Admission, Refused> {
        admit_invocation(
            &invoker.authority(policy),
            invoker.asker(true),
            invocation,
            &Context::at_secs(10),
        )
        .map(|(admission, _)| admission)
    }

    #[test]
    fn an_invokers_standing_decides_as_its_authority_does() {
        let policy = policy();
        let node = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
        let cx = Context::at_secs(10);
        for invoker in [
            Invoker::person("editor"),
            Invoker::person("editor").on_foreign_device(true),
            Invoker::session_agent(WORKSPACE, "scribe", "editor"),
            Invoker::delegated(Subject::bot("helper"), "owner", Vec::new()),
        ] {
            let standing = invoker.clone().standing(policy.clone()).unwrap();
            for kind in [ActionKind::Read, ActionKind::Undoable, ActionKind::Outside] {
                assert_eq!(
                    standing.answer(&node, kind, &cx),
                    invoker.authority(&policy).decide(&node, kind, &cx).answer,
                    "{invoker:?} {kind:?}"
                );
            }
        }
    }

    #[test]
    fn whoever_acts_for_a_person_is_asked_before_an_attended_action_wherever_it_runs() {
        let policy = policy();
        let node = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
        let attended = Invocation {
            attended: true,
            ..invocation(&node, ActionKind::Read)
        };
        assert_eq!(
            admitted(&Invoker::person("editor"), &policy, attended),
            Ok(Admission::Runs)
        );
        assert_eq!(
            admitted(
                &Invoker::session_agent(WORKSPACE, "scribe", "editor"),
                &policy,
                attended
            ),
            Ok(Admission::AsksFirst)
        );
    }

    #[test]
    fn an_agents_ask_reaches_the_person_only_where_the_host_reaches_them() {
        let policy = policy();
        let node = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
        let agent = Invoker::session_agent(WORKSPACE, "scribe", "editor");
        let outside = invocation(&node, ActionKind::Outside);
        let cx = Context::at_secs(10);
        let authority = agent.authority(&policy);
        assert_eq!(
            admit_invocation(&authority, agent.asker(true), outside, &cx).map(|(a, _)| a),
            Ok(Admission::AsksFirst),
            "an agent reaching outside asks its person first"
        );
        assert_eq!(
            admit_invocation(&authority, agent.asker(false), outside, &cx).map(|(a, _)| a),
            Err(Refused::Action),
            "nobody to ask refuses"
        );
        let person = Invoker::person("editor");
        assert!(person.asker(false).reachable, "a person answers themselves");
    }

    #[test]
    fn a_run_of_an_issue_is_decided_by_grants_naming_the_issue() {
        let mut policy = policy();
        let app = TreePath::workspace(WORKSPACE).app("so.tilo.tasks");
        policy.grants.push(Grant::new(
            "issue-may-edit",
            Subject::Task("issue-1".into()),
            app.clone(),
            KindSet::ALL,
            Answer::Allow,
            Subject::person("owner"),
        ));
        let agent = Invoker::delegated(Subject::agent("scribe"), "owner", Vec::new());
        let cx = Context::at_secs(10);
        assert_eq!(
            agent
                .authority(&policy)
                .decide(&app, ActionKind::Undoable, &cx)
                .answer,
            Answer::Deny
        );
        let working = agent.working_on("issue-1");
        assert_eq!(
            working
                .authority(&policy)
                .decide(&app, ActionKind::Undoable, &cx)
                .answer,
            Answer::Allow
        );
    }

    #[test]
    fn a_step_going_further_than_its_invocation_asks_first() {
        let policy = policy();
        let node = TreePath::workspace(WORKSPACE).app("so.tilo.notes");
        let read = Reach {
            kind: ActionKind::Read,
            privileged: false,
        };
        let edit = Invocation {
            ceiling: Some(read),
            ..invocation(&node, ActionKind::Undoable)
        };
        assert_eq!(
            admitted(&Invoker::person("owner"), &policy, edit),
            Ok(Admission::AsksFirst)
        );
    }
}
