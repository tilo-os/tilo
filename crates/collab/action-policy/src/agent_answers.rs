//! What a person has told one of their agents, or the work on an issue, it
//! may do: allowed, asks first, or not allowed, for each kind of action, scoped to an application, one kind
//! of resource in it, or one action.
//!
//! These are ordinary grants on the workspace, named so that setting an answer
//! again replaces it instead of adding a second one. Without one, a run a
//! person started takes the agent delegation's answer
//! ([`crate::roles::agent_grants`]), which is [`default_answer`], the
//! default a person sees before choosing; an agent acting inside a turn is
//! decided the same way, bounded by the cap its record narrows.
//!
//! An action whose contract asks for confirmation (it sends, publishes or
//! deletes for good) keeps asking an agent's person even where an
//! application- or kind-wide answer allows its kind: only an answer given for
//! that very action ([`AnswerScope::Action`], what "Always allow this" gives)
//! lifts it. Allowing "changes" in Mail therefore never quietly lets an agent
//! empty the Trash.

use crate::{
    ActionKind, Answer, Basis, Change, Decision, Grant, GrantId, Grantee, KindSet, Level, TreePath,
};

/// The id prefix of every answer grant.
pub const PREFIX: &str = "agent-answer:";

/// Where an answer applies, below the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnswerScope {
    /// Every action of an application.
    App { app: String },
    /// Every action on one kind of resource of an application.
    Kind { app: String, kind: String },
    /// One action, on any resource of its kind.
    Action {
        app: String,
        kind: String,
        action: String,
    },
}

impl AnswerScope {
    #[must_use]
    pub fn app(app: impl Into<String>) -> Self {
        Self::App { app: app.into() }
    }

    #[must_use]
    pub fn kind(app: impl Into<String>, kind: impl Into<String>) -> Self {
        Self::Kind {
            app: app.into(),
            kind: kind.into(),
        }
    }

    #[must_use]
    pub fn action(
        app: impl Into<String>,
        kind: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self::Action {
            app: app.into(),
            kind: kind.into(),
            action: action.into(),
        }
    }

    /// The node this scope stands for in `workspace`. An action's node may pass
    /// through the resource's fields, so one action is matched at any depth
    /// below its kind.
    #[must_use]
    pub fn node(&self, workspace: &str) -> TreePath {
        let workspace = TreePath::workspace(workspace);
        match self {
            Self::App { app } => workspace.app(app),
            Self::Kind { app, kind } => workspace.app(app).kind(kind),
            Self::Action { app, kind, action } => {
                workspace.app(app).kind(kind).rest().action(action)
            }
        }
    }

    /// The scope a grant's node stands for, when it is one.
    #[must_use]
    pub fn of_node(node: &TreePath) -> Option<Self> {
        use crate::Segment;
        match node.segments() {
            [
                Segment::Name(Level::Workspace, _),
                Segment::Name(Level::App, app),
            ] => Some(Self::app(app)),
            [
                Segment::Name(Level::Workspace, _),
                Segment::Name(Level::App, app),
                Segment::Name(Level::Kind, kind),
            ] => Some(Self::kind(app, kind)),
            [
                Segment::Name(Level::Workspace, _),
                Segment::Name(Level::App, app),
                Segment::Name(Level::Kind, kind),
                Segment::Rest,
                Segment::Name(Level::Action, action),
            ] => Some(Self::action(app, kind, action)),
            _ => None,
        }
    }

    /// The scope as one key, which [`Self::parse`] reads back: the app's id,
    /// and the kind and the action a narrower scope names.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::App { app } => format!("app:{app}"),
            Self::Kind { app, kind } => format!("kind:{app}:{kind}"),
            Self::Action { app, kind, action } => format!("action:{app}:{kind}:{action}"),
        }
    }

    /// The scope a [`Self::key`] names, if it names one.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        let mut parts = key.split(':');
        match (
            parts.next()?,
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            ("app", Some(app), None, None, None) if !app.is_empty() => Some(Self::app(app)),
            ("kind", Some(app), Some(kind), None, None) if !app.is_empty() && !kind.is_empty() => {
                Some(Self::kind(app, kind))
            }
            ("action", Some(app), Some(kind), Some(action), None)
                if !app.is_empty() && !kind.is_empty() && !action.is_empty() =>
            {
                Some(Self::action(app, kind, action))
            }
            _ => None,
        }
    }
}

#[must_use]
pub fn grant_id(agent: &str, scope: &AnswerScope, kind: ActionKind) -> GrantId {
    GrantId(format!("{PREFIX}{agent}:{}:{kind}", scope.key()))
}

/// Whether `grant` is an answer: given an agent, for one of its runs.
#[must_use]
pub fn is_answer(grant: &Grant) -> bool {
    matches!(grant.grantee, Grantee::Member(crate::MemberId::Agent(_)))
        && grant.id.as_str().starts_with(PREFIX)
}

/// The grant giving `agent` `answer` for actions of `kind` at `scope` in
/// `workspace`, set by `granted_by`.
#[must_use]
pub fn grant(
    workspace: &str,
    agent: &str,
    (scope, kind): (&AnswerScope, ActionKind),
    answer: Answer,
    granted_by: Grantee,
) -> Grant {
    answer_grant(
        workspace,
        Grantee::agent(agent),
        (scope, kind),
        answer,
        granted_by,
    )
}

/// The grant giving `delegate` (an agent) `answer` for actions of `kind` at
/// `scope` in `workspace`, set by `granted_by`.
#[must_use]
pub fn answer_grant(
    workspace: &str,
    delegate: Grantee,
    (scope, kind): (&AnswerScope, ActionKind),
    answer: Answer,
    granted_by: Grantee,
) -> Grant {
    let key = match &delegate {
        Grantee::Member(crate::MemberId::Agent(agent)) => agent.clone(),
        other => other.to_string(),
    };
    Grant::new(
        grant_id(&key, scope, kind).0,
        delegate,
        scope.node(workspace),
        KindSet::of(kind),
        answer,
        granted_by,
    )
}

/// The changes setting `agent`'s answer for `kind` at `scope` in a workspace
/// holding `held`: the earlier answer is revoked, so the new one replaces
/// it. `None` clears the answer, returning the scope to what the agent's
/// admission and broader answers say.
#[must_use]
pub fn set(
    workspace: &str,
    agent: &str,
    (scope, kind): (&AnswerScope, ActionKind),
    answer: Option<Answer>,
    (granted_by, held): (Grantee, &[Grant]),
) -> Vec<Change> {
    let id = grant_id(agent, scope, kind);
    held.iter()
        .filter(|grant| grant.id == id)
        .map(|grant| Change::Revoke(grant.id.clone()))
        .chain(answer.map(|answer| {
            Change::Grant(grant(workspace, agent, (scope, kind), answer, granted_by))
        }))
        .collect()
}

/// The answer an agent starts with for an action of `kind`, before anyone
/// chose: reading and undoable changes are allowed, reaching outside the
/// document asks first, and an action whose contract asks for confirmation
/// (`consequential`: it deletes for good, publishes, crosses a security
/// boundary) asks first whatever its kind.
#[must_use]
pub const fn default_answer(kind: ActionKind, consequential: bool) -> Answer {
    match kind {
        _ if consequential => Answer::Ask,
        ActionKind::Read | ActionKind::Undoable => Answer::Allow,
        ActionKind::Outside => Answer::Ask,
    }
}

/// The answer a delegate's `decision` gives an action, held to the action's
/// contract: an action that asks for confirmation (`consequential`) asks
/// first where the decision allows it, unless the decision is an answer
/// given for that one action.
#[must_use]
pub fn held_to_contract(decision: &Decision<'_>, consequential: bool) -> Answer {
    if !consequential || decision.answer != Answer::Allow {
        return decision.answer;
    }
    match decision.basis {
        Basis::Grant(grant)
            if is_answer(grant)
                && matches!(
                    AnswerScope::of_node(&grant.node),
                    Some(AnswerScope::Action { .. })
                ) =>
        {
            Answer::Allow
        }
        _ => Answer::Ask,
    }
}
