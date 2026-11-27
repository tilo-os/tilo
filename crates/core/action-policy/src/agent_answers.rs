//! What a person has told one of their agents, the work on an issue, or an
//! app, it may do: allowed, asks first, or not allowed, for each kind of
//! action, scoped to an application, one kind of resource in it, or one
//! action. An app's answer is what it may take of another app's actions:
//! "Recorder may post to Chat" is asked and kept like an agent's.
//!
//! These are ordinary grants on the workspace, named so that setting an answer
//! again replaces it instead of adding a second one. Without one, a run a
//! person started takes the agent delegation's answer
//! ([`tilo_member_types::Acting::handed_preset`]), which is [`default_answer`], the
//! default a person sees before choosing; an agent acting inside a turn is
//! decided the same way, bounded by the cap its resource narrows.
//!
//! An action whose contract asks for confirmation (it sends, publishes or
//! deletes for good) keeps asking an agent's person even where an
//! application- or kind-wide answer allows its kind: only an answer given for
//! that very action ([`AnswerScope::Action`], what "Always allow this" gives)
//! lifts it. Allowing "changes" in Mail therefore never quietly lets an agent
//! empty the Trash.

use crate::{Answer, Basis, Decision, Grant, GrantId, Grantee, Level, TreePath};
use tilo_action_types::{ActionKind, KindSet};

/// The id prefix of every answer grant.
pub const PREFIX: &str = "agent-answer:";

/// Where an answer applies, below the workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AnswerScope {
    /// Every action in the workspace, whatever app or resource: what a stop is
    /// ([`stop_grant`]).
    Workspace,
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
    /// One action, on one resource of its kind: what "always" on one site gives, leaving
    /// every other site to ask.
    Resource {
        app: String,
        kind: String,
        resource: String,
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

    #[must_use]
    pub fn resource(
        app: impl Into<String>,
        kind: impl Into<String>,
        resource: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self::Resource {
            app: app.into(),
            kind: kind.into(),
            resource: resource.into(),
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
            Self::Workspace => workspace,
            Self::App { app } => workspace.app(app),
            Self::Kind { app, kind } => workspace.app(app).kind(kind),
            Self::Action { app, kind, action } => {
                workspace.app(app).kind(kind).rest().action(action)
            }
            Self::Resource {
                app,
                kind,
                resource,
                action,
            } => workspace
                .app(app)
                .kind(kind)
                .resource(resource)
                .rest()
                .action(action),
        }
    }

    /// The scope a grant's node stands for, when it is one.
    #[must_use]
    pub fn of_node(node: &TreePath) -> Option<Self> {
        use crate::Segment;
        match node.segments() {
            [Segment::Name(Level::Workspace, _)] => Some(Self::Workspace),
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
            [
                Segment::Name(Level::Workspace, _),
                Segment::Name(Level::App, app),
                Segment::Name(Level::Kind, kind),
                Segment::Name(Level::Resource, resource),
                Segment::Rest,
                Segment::Name(Level::Action, action),
            ] => Some(Self::resource(app, kind, resource, action)),
            _ => None,
        }
    }

    /// The scope as one key, which [`Self::parse`] reads back: the app's id,
    /// and the kind and the action a narrower scope names.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Workspace => "workspace".to_owned(),
            Self::App { app } => format!("app:{app}"),
            Self::Kind { app, kind } => format!("kind:{app}:{kind}"),
            Self::Action { app, kind, action } => format!("action:{app}:{kind}:{action}"),
            Self::Resource {
                app,
                kind,
                resource,
                action,
            } => format!("resource:{app}:{kind}:{resource}:{action}"),
        }
    }

    /// The scope a [`Self::key`] names, if it names one.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        // A resource's id may hold `:` itself (a site's address): it is all between the kind
        // and the action, which is last.
        if let Some(rest) = key.strip_prefix("resource:") {
            let mut parts = rest.splitn(3, ':');
            let (app, kind, rest) = (parts.next()?, parts.next()?, parts.next()?);
            let (resource, action) = rest.rsplit_once(':')?;
            return [app, kind, resource, action]
                .iter()
                .all(|part| !part.is_empty())
                .then(|| Self::resource(app, kind, resource, action));
        }
        let mut parts = key.split(':');
        match (
            parts.next()?,
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) {
            ("workspace", None, None, None, None) => Some(Self::Workspace),
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

/// What an answer for the agent `agent` acting for `person` is named by. An
/// agent is the pair: `scout` acting for one person and `scout` acting for
/// another are two agents, and each one's answers are kept apart from the
/// other's, so neither replaces nor takes back the other's.
#[must_use]
pub fn agent_key(agent: &str, person: &str) -> String {
    format!("{person}/{agent}")
}

/// The id of the answer that stops `agent` acting for `person` in a workspace
/// ([`stop_grant`]).
#[must_use]
pub fn stop_grant_id(agent: &str, person: &str) -> GrantId {
    GrantId(format!(
        "{PREFIX}{}:workspace:all",
        agent_key(agent, person)
    ))
}

/// The answer that stops `agent` acting for `person` in `workspace` until
/// `person` lets it go on: every kind denied on the whole workspace, given by
/// them. No more specific grant of the agent lifts it
/// ([`crate::WorkspacePolicy::party_of_agent`]), so a stop holds whatever the
/// agent's certificate or its person's earlier answers allow.
#[must_use]
#[track_caller]
pub fn stop_grant(workspace: &str, agent: &str, person: &str) -> Grant {
    Grant::new(
        stop_grant_id(agent, person).0,
        Grantee::agent(agent, person),
        AnswerScope::Workspace.node(workspace),
        KindSet::ALL,
        Answer::Deny,
        Grantee::person(person),
    )
}

/// Whether `grant` is an answer: given to an agent or an app, or for one
/// issue's runs.
#[must_use]
pub fn is_answer(grant: &Grant) -> bool {
    let id = grant.id.as_str();
    (id.starts_with(PREFIX)
        && matches!(
            grant.grantee,
            Grantee::Member(tilo_member_types::MemberId::Agent { .. }) | Grantee::App(_)
        ))
        || crate::is_session_answer(grant)
}

/// The grant giving `agent` acting for `person` `answer` for actions of `kind`
/// at `scope` in `workspace`, set by `granted_by`.
#[must_use]
#[track_caller]
pub fn grant(
    workspace: &str,
    (agent, person): (&str, &str),
    (scope, kind): (&AnswerScope, ActionKind),
    answer: Answer,
    granted_by: Grantee,
) -> Grant {
    answer_grant(
        workspace,
        Grantee::agent(agent, person),
        (scope, kind),
        answer,
        granted_by,
    )
}

/// The grant giving `delegate` (an agent or an app) `answer` for actions of
/// `kind` at `scope` in `workspace`, set by `granted_by`.
#[must_use]
pub fn answer_grant(
    workspace: &str,
    delegate: Grantee,
    (scope, kind): (&AnswerScope, ActionKind),
    answer: Answer,
    granted_by: Grantee,
) -> Grant {
    let key = match &delegate {
        Grantee::Member(tilo_member_types::MemberId::Agent { id, person }) => {
            agent_key(id, person.as_str())
        }
        Grantee::App(app) => format!("app:{app}"),
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

/// The app a workspace's own actions are published under: its members, roles,
/// invitations and settings.
pub const WORKSPACE_APP: &str = "tilo.workspace";
/// The provider a server publishes a person's own workspaces there under:
/// creating, listing, entering and deleting them.
pub const ACCOUNT_APP: &str = "tilo.host";

/// Whether an agent may be given `answer` for actions of `kind` at `scope`; `None` for a node no
/// answer scope names, such as the whole workspace.
///
/// What the person's own account holds is theirs to allow: their workspaces on
/// a server ([`ACCOUNT_APP`]) and a workspace's own actions ([`WORKSPACE_APP`]).
/// The server holds every such call to what the person may do themselves, so
/// their agent never reaches further than they do. Any other app's outside
/// effect reaches other people's data, accounts or machines (it sends,
/// publishes, calls a service or executes a command), so it is carried out
/// unasked only where its person allowed that one action
/// ([`AnswerScope::Action`], what "Always allow this" gives): never for a whole
/// app or kind of resource, which would let actions they never looked at go
/// unasked as well.
///
/// What every list of an agent's answers offers, and what the workspace's bound
/// is held to.
#[must_use]
pub fn may_give(scope: Option<&AnswerScope>, kind: ActionKind, answer: Answer) -> bool {
    kind != ActionKind::Outside
        || answer != Answer::Allow
        || scope.is_some_and(|scope| match scope {
            // One action, or one action on one resource: an answer the person gave for what
            // they looked at.
            AnswerScope::Action { .. } | AnswerScope::Resource { .. } => true,
            AnswerScope::App { app } | AnswerScope::Kind { app, .. } => {
                [ACCOUNT_APP, WORKSPACE_APP].contains(&app.as_str())
            }
            // A allow covering the whole workspace is never one the person looked at.
            AnswerScope::Workspace => false,
        })
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
                    Some(AnswerScope::Action { .. } | AnswerScope::Resource { .. })
                ) =>
        {
            Answer::Allow
        }
        _ => Answer::Ask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer given on one resource leaves every other resource of its kind asking, and
    /// reads back from its key and its node, a resource id with `:` in it included.
    #[test]
    fn an_answer_on_one_resource_leaves_another_asking() {
        let scope = AnswerScope::resource("browser", "site", "https://a.example", "download");
        let node = scope.node("w");
        let path = |site: &str| {
            TreePath::workspace("w")
                .app("browser")
                .kind("site")
                .resource(site)
                .action("download")
        };
        assert!(node.covers(&path("https://a.example")));
        assert!(!node.covers(&path("https://b.example")));
        assert_eq!(AnswerScope::parse(&scope.key()), Some(scope.clone()));
        assert_eq!(AnswerScope::of_node(&node), Some(scope.clone()));
        // An outside action allowed on one resource is an answer for what its person looked
        // at, as one allowed for one action is; a whole app's is not.
        assert!(may_give(Some(&scope), ActionKind::Outside, Answer::Allow));
        assert!(!may_give(
            Some(&AnswerScope::app("browser")),
            ActionKind::Outside,
            Answer::Allow
        ));
    }
}
