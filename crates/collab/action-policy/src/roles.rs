//! Presets: named grant sets. Roles are the built-in presets on a room; an
//! agent's default delegation and the foreign-device bound are presets too.

use serde::{Deserialize, Serialize};

use crate::{Answer, Grant, GrantId, KindSet, Level, Lifetime, Role, Subject, TreePath};

/// The kind id of the room's own resource. Its actions are the room's
/// governance: membership, roles, grants and settings.
pub const ROOM_KIND: &str = "tilo.room";

/// The field holding comments, wherever a model keeps them. The Commenter
/// preset covers it at any depth, so field edits and verbs published on it are
/// what a commenter may do.
pub const COMMENTS_FIELD: &str = "comments";

/// The room's governance actions.
pub mod governance {
    /// Invite someone, carrying at most the inviter's role.
    pub const INVITE: &str = "invite";
    /// Admit a member directly.
    pub const ADD_MEMBER: &str = "add-member";
    /// Remove a member. The owner cannot be removed.
    pub const REMOVE_MEMBER: &str = "remove-member";
    /// Change a member's role.
    pub const SET_ROLE: &str = "set-role";
    /// Add a grant.
    pub const GRANT: &str = "grant";
    /// Revoke a grant.
    pub const REVOKE: &str = "revoke";
    /// Change the room's settings.
    pub const SETTINGS: &str = "settings";
    /// Replace the room's history with a checkpoint.
    pub const COMPACT: &str = "compact";
    /// Remove an earlier change from the room's history.
    pub const REDACT: &str = "redact";

    pub const ALL: [&str; 9] = [
        INVITE,
        ADD_MEMBER,
        REMOVE_MEMBER,
        SET_ROLE,
        GRANT,
        REVOKE,
        SETTINGS,
        COMPACT,
        REDACT,
    ];
}

/// The kind of every governance action. Changing who may do what reaches
/// beyond the document — it decides what other people see and do — so an
/// agent's default asks before it, and the wire never lets one through on an
/// answer that would need asking.
pub const GOVERNANCE_KIND: crate::ActionKind = crate::ActionKind::Outside;

/// The room's own resource: `room:<id>/kind:tilo.room`.
#[must_use]
pub fn room_resource(room: &str) -> TreePath {
    TreePath::room(room).kind(ROOM_KIND)
}

/// The node of one governance action.
#[must_use]
pub fn governance_action(room: &str, action: &str) -> TreePath {
    room_resource(room).action(action)
}

/// Where comments live in a room, at any depth.
#[must_use]
pub fn comments(room: &str) -> TreePath {
    TreePath::room(room)
        .rest()
        .push(Level::Field, COMMENTS_FIELD)
}

/// One rule of a preset: a grant without its subject.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Rule {
    pub node: TreePath,
    pub kinds: KindSet,
    pub answer: Answer,
}

impl Rule {
    #[must_use]
    pub const fn new(node: TreePath, kinds: KindSet, answer: Answer) -> Self {
        Self {
            node,
            kinds,
            answer,
        }
    }
}

/// A named grant set people pick instead of writing grants one by one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub description: String,
    pub rules: Vec<Rule>,
}

/// A preset that cannot be assigned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetError {
    Unnamed,
    Empty,
    /// A rule names no kind.
    NoKinds(usize),
    /// Two rules on the same node share a kind with different answers.
    Contradicts(usize, usize),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unnamed => f.write_str("a preset needs a name"),
            Self::Empty => f.write_str("a preset needs at least one rule"),
            Self::NoKinds(index) => write!(f, "rule {index} covers no kind of action"),
            Self::Contradicts(first, second) => {
                write!(
                    f,
                    "rules {first} and {second} give one node different answers"
                )
            }
        }
    }
}

impl std::error::Error for PresetError {}

impl Preset {
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>, rules: Vec<Rule>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            rules,
        }
    }

    /// # Errors
    /// When the preset is unnamed, empty, has a rule covering no kind, or two
    /// rules contradict on one node.
    pub fn validate(&self) -> Result<(), PresetError> {
        if self.name.trim().is_empty() {
            return Err(PresetError::Unnamed);
        }
        if self.rules.is_empty() {
            return Err(PresetError::Empty);
        }
        for (index, rule) in self.rules.iter().enumerate() {
            if rule.kinds.is_empty() {
                return Err(PresetError::NoKinds(index));
            }
            for (other_index, other) in self.rules.iter().enumerate().skip(index.saturating_add(1))
            {
                let shared = rule.kinds.iter().any(|kind| other.kinds.contains(kind));
                if rule.node == other.node && shared && rule.answer != other.answer {
                    return Err(PresetError::Contradicts(index, other_index));
                }
            }
        }
        Ok(())
    }

    /// The preset's grants for `subject`. Ids are `<prefix>:<index>`.
    #[must_use]
    pub fn assign(
        &self,
        prefix: &str,
        subject: &Subject,
        granted_by: &Subject,
        lifetime: &Lifetime,
    ) -> Vec<Grant> {
        self.rules
            .iter()
            .enumerate()
            .map(|(index, rule)| Grant {
                id: GrantId(format!("{prefix}:{index}")),
                subject: subject.clone(),
                node: rule.node.clone(),
                kinds: rule.kinds,
                answer: rule.answer,
                lifetime: lifetime.clone(),
                granted_by: granted_by.clone(),
            })
            .collect()
    }
}

/// The rules a role carries in `room`.
///
/// | Role | Grants on the room |
/// |---|---|
/// | Viewer | Read: Allow |
/// | Commenter | Read: Allow; comments: Allow (Undoable) |
/// | Editor | Read, Undoable: Allow; Outside: Ask; governance: Deny |
/// | Owner | everything: Allow |
#[must_use]
pub fn role_rules(role: Role, room: &str) -> Vec<Rule> {
    let room_node = TreePath::room(room);
    match role {
        Role::Viewer => vec![Rule::new(room_node, KindSet::READ, Answer::Allow)],
        Role::Commenter => vec![
            Rule::new(room_node, KindSet::READ, Answer::Allow),
            Rule::new(comments(room), KindSet::UNDOABLE, Answer::Allow),
        ],
        Role::Editor => vec![
            Rule::new(room_node.clone(), KindSet::READ_UNDOABLE, Answer::Allow),
            Rule::new(room_node, KindSet::OUTSIDE, Answer::Ask),
            Rule::new(
                room_resource(room),
                KindSet::UNDOABLE.union(KindSet::OUTSIDE),
                Answer::Deny,
            ),
        ],
        Role::Owner => vec![Rule::new(room_node, KindSet::ALL, Answer::Allow)],
    }
}

/// The preset a role stands for in `room`.
#[must_use]
pub fn role_preset(role: Role, room: &str) -> Preset {
    let description = match role {
        Role::Viewer => "Can read everything in the room.",
        Role::Commenter => "Can read everything and comment.",
        Role::Editor => {
            "Can read and make undoable changes; outside effects ask first; cannot govern the room."
        }
        Role::Owner => "Can do everything, including governing the room.",
    };
    Preset::new(role.name(), description, role_rules(role, room))
}

/// Every role's grants for a new room, given by its owner. Ids are
/// `role:<role>:<index>`.
#[must_use]
pub fn room_role_grants(room: &str, granted_by: &Subject) -> Vec<Grant> {
    Role::ALL
        .into_iter()
        .flat_map(|role| {
            role_preset(role, room).assign(
                &format!("role:{}", role.name().to_ascii_lowercase()),
                &Subject::Role(role),
                granted_by,
                &Lifetime::UntilRevoked,
            )
        })
        .collect()
}

/// An agent's default delegation within the scope a person gives it: Read and
/// Undoable allowed, Outside asks.
#[must_use]
pub fn agent_default(scope: &[TreePath]) -> Preset {
    Preset::new(
        "Agent default",
        "Can read and make undoable changes; outside effects ask first.",
        scope
            .iter()
            .flat_map(|node| {
                [
                    Rule::new(node.clone(), KindSet::READ_UNDOABLE, Answer::Allow),
                    Rule::new(node.clone(), KindSet::OUTSIDE, Answer::Ask),
                ]
            })
            .collect(),
    )
}

/// The delegation a delegated agent `agent` of `person` carries in `room`:
/// the Editor preset. The agent acts with its person's grants bounded by it,
/// so it changes what its person may change, never more than an Editor, and
/// never the room's governance.
#[must_use]
pub fn agent_delegation(room: &str, agent: &str, person: &str) -> crate::Bound {
    crate::Bound::delegation(
        Subject::Agent(agent.to_owned()),
        &role_preset(Role::Editor, room),
        person,
    )
}

/// What an agent admitted to a room may do there: the room-level grants it
/// receives. Never a human workspace role.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPreset {
    Observer,
    Contributor,
    Operator,
    Custom,
}

impl AgentPreset {
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "observer" => Some(Self::Observer),
            "contributor" => Some(Self::Contributor),
            "operator" => Some(Self::Operator),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Contributor => "contributor",
            Self::Operator => "operator",
            Self::Custom => "custom",
        }
    }

    /// The grants admitting `agent` to `room` with this preset, given by
    /// `granted_by`. Ids are `agent-member:<agent>:<preset>[:<n>]`.
    #[must_use]
    pub fn grants(self, room: &str, agent: &str, granted_by: &Subject) -> Vec<Grant> {
        let id = format!("agent-member:{agent}:{}", self.id());
        let subject = Subject::agent(agent);
        let grant = |suffix: &str, node: TreePath, kinds: KindSet, answer: Answer| {
            Grant::new(
                format!("{id}{suffix}"),
                subject.clone(),
                node,
                kinds,
                answer,
                granted_by.clone(),
            )
        };
        let read = grant("", TreePath::room(room), KindSet::READ, Answer::Allow);
        // An admitted agent takes part in the room; it never governs it.
        let no_governance = grant(
            ":no-governance",
            room_resource(room),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
        );
        match self {
            Self::Observer | Self::Custom => vec![read, no_governance],
            Self::Contributor => vec![
                read,
                grant(
                    ":comments",
                    comments(room),
                    KindSet::UNDOABLE,
                    Answer::Allow,
                ),
                no_governance,
            ],
            Self::Operator => vec![
                grant(
                    "",
                    TreePath::room(room),
                    KindSet::READ_UNDOABLE,
                    Answer::Allow,
                ),
                grant(
                    ":outside",
                    TreePath::room(room),
                    KindSet::OUTSIDE,
                    Answer::Ask,
                ),
                no_governance,
            ],
        }
    }

    /// The changes admitting `agent` with this preset to a room holding
    /// `held`: its earlier admission, under any preset, is revoked first, so
    /// admitting it again changes its preset rather than adding to it.
    #[must_use]
    pub fn admission(
        self,
        room: &str,
        agent: &str,
        granted_by: &Subject,
        held: &[Grant],
    ) -> Vec<crate::Change> {
        let prefix = format!("agent-member:{agent}:");
        let subject = Subject::agent(agent);
        held.iter()
            .filter(|grant| grant.subject == subject && grant.id.as_str().starts_with(&prefix))
            .map(|grant| crate::Change::Revoke(grant.id.clone()))
            .chain(
                self.grants(room, agent, granted_by)
                    .into_iter()
                    .map(crate::Change::Grant),
            )
            .collect()
    }
}

/// The grant making application `app` an author of `room`: it reads and
/// changes the room's content as the person using it, and reaches nothing
/// outside it. Given when a person who may change the room opens the app.
#[must_use]
pub fn app_authorship(room: &str, app: &str, granted_by: Subject) -> Grant {
    Grant::new(
        format!("app-author:{app}"),
        Subject::App(app.to_owned()),
        TreePath::room(room),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        granted_by,
    )
}

/// What a person may do from a device that is not theirs, in any room: read
/// and change content, ask first before reaching outside it, never govern.
#[must_use]
pub fn foreign_device_bound() -> Vec<Grant> {
    let issuer = Subject::ForeignDevice;
    let mut grants = vec![
        Grant::new(
            "foreign-device:content",
            Subject::ForeignDevice,
            TreePath::root(),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            issuer.clone(),
        ),
        Grant::new(
            "foreign-device:outside",
            Subject::ForeignDevice,
            TreePath::root(),
            KindSet::OUTSIDE,
            Answer::Ask,
            issuer.clone(),
        ),
    ];
    grants.push(Grant::new(
        "foreign-device:governance",
        Subject::ForeignDevice,
        TreePath::root().any(Level::Room).kind(ROOM_KIND),
        KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        Answer::Deny,
        issuer,
    ));
    grants
}
