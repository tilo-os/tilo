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

/// The field holding reactions, wherever a model keeps them
/// (`tilo_resource::Reactions`). Reacting is commenting's smallest form, so
/// the Commenter preset covers it at any depth too.
pub const REACTIONS_FIELD: &str = "reactions";

/// The field node every app's moderation actions sit under (acting on other
/// people's content: removing their message, locking a channel). One rule on
/// it covers every app's moderation, so presets need no list of verbs.
pub const MODERATION_FIELD: &str = "moderation";

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
    /// Remove a member and keep them out until unbanned: a removal plus the
    /// `block:<person>` Deny.
    pub const BAN: &str = "ban";
    /// Lift a ban: revoke the person's `block:<person>` Deny.
    pub const UNBAN: &str = "unban";
    /// Stop a member changing anything until a time: the expiring
    /// `restrict:<person>` Deny. They keep reading the room.
    pub const RESTRICT: &str = "restrict";
    /// Lift a restriction before it lapses.
    pub const UNRESTRICT: &str = "unrestrict";
    /// Replace the room's history with a checkpoint.
    pub const COMPACT: &str = "compact";
    /// Remove an earlier change from the room's history.
    pub const REDACT: &str = "redact";

    /// Install a bot, or update an installed bot's program: its row, its
    /// grants and its device in one governed change.
    pub const INSTALL_BOT: &str = "install-bot";
    /// Withdraw a bot and revoke its grants; its row stays so its history
    /// stays attributed.
    pub const UNINSTALL_BOT: &str = "uninstall-bot";
    /// Change whether a bot may post under other names.
    pub const SET_BOT_APPEARANCE: &str = "set-bot-appearance";
    /// Stop a bot from starting runs.
    pub const PAUSE_BOT: &str = "pause-bot";
    /// Let a paused bot start runs again.
    pub const RESUME_BOT: &str = "resume-bot";
    /// Become a bot's installer, after reviewing its grant.
    pub const ADOPT_BOT: &str = "adopt-bot";
    /// Start, end, restart or moderate one of the room's calls. A call is
    /// room governance (who may speak, who is denied), so no preset below
    /// the owner's holds it: the owner, and members the room grants it, run
    /// calls, and any member joins an active one.
    pub const CALL: &str = "call";

    pub const ALL: [&str; 20] = [
        INVITE,
        ADD_MEMBER,
        REMOVE_MEMBER,
        BAN,
        UNBAN,
        RESTRICT,
        UNRESTRICT,
        SET_ROLE,
        GRANT,
        REVOKE,
        SETTINGS,
        COMPACT,
        REDACT,
        INSTALL_BOT,
        UNINSTALL_BOT,
        SET_BOT_APPEARANCE,
        PAUSE_BOT,
        RESUME_BOT,
        ADOPT_BOT,
        CALL,
    ];

    /// Actions that exercise authority over the room or others in it. The
    /// person's own grants must allow them: an Ask there is no authority the
    /// person's confirmation could supply. Pausing is not among them, so a
    /// moderator can stop a misbehaving bot without being able to widen it.
    pub const PRIVILEGED: [&str; 4] = [INSTALL_BOT, UNINSTALL_BOT, SET_BOT_APPEARANCE, ADOPT_BOT];

    #[must_use]
    pub fn is_privileged(action: &str) -> bool {
        PRIVILEGED.contains(&action)
    }
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

#[must_use]
pub fn governance_action(room: &str, action: &str) -> TreePath {
    room_resource(room).action(action)
}

/// Every governance action of `room`: `room:<id>/kind:tilo.room/action:*`.
/// More specific than the room's resource, where the Editor's rule denies
/// governance, so a grant here decides each governance action whatever the
/// grantee's role says about the room's resource.
#[must_use]
pub fn governance_actions(room: &str) -> TreePath {
    room_resource(room).any(Level::Action)
}

/// The id of the grant that makes `person` an administrator of a room.
#[must_use]
pub fn administrator_grant_id(person: &str) -> GrantId {
    GrantId(format!("room-admin-v1:{person}"))
}

/// The grant that makes `person` an administrator of `room`, given by its
/// owner `granted_by`: every governance action allowed, on
/// [`governance_actions`].
///
/// An administrator stays a member with a role, an Editor as the room's
/// governance writes them, and that role still bounds them: an invite or a
/// role they hand out is never above it ([`crate::Refusal::AboveActorRole`]),
/// a membership action reaches only those ranked below it
/// ([`crate::Refusal::NotBelowActor`]), and a grant they give never exceeds
/// what they hold ([`crate::Refusal::AboveActorGrant`]). A ban or a live
/// restriction on them stops their governance with the rest of their
/// changes ([`crate::RoomPolicy::check_action`]).
///
/// Rooms written before the grant took this node held it on
/// [`room_resource`], where the Editor's own Deny at the same node outranked
/// it; [`is_legacy_administrator_grant`] finds those so the governance writer
/// rewrites them.
#[must_use]
pub fn administrator_grant(room: &str, person: &str, granted_by: Subject) -> Grant {
    Grant {
        id: administrator_grant_id(person),
        subject: Subject::Person(person.to_owned()),
        node: governance_actions(room),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// The id of the grant that lets an administrator moderate `person`'s room.
#[must_use]
pub fn administrator_moderation_grant_id(person: &str) -> GrantId {
    GrantId(format!("room-admin-moderation-v1:{person}"))
}

/// The grant that lets `person`, an administrator of `room`, moderate it:
/// every moderation action allowed, as a Moderator may, given beside
/// [`administrator_grant`] by its giver. It sits one level below
/// [`moderation`], where an Editor's own role denies, so it decides every
/// moderation action whatever that role says.
#[must_use]
pub fn administrator_moderation_grant(room: &str, person: &str, granted_by: Subject) -> Grant {
    Grant {
        id: administrator_moderation_grant_id(person),
        subject: Subject::Person(person.to_owned()),
        node: moderation(room).any(Level::Action),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// Whether `grant` is an administrator's grant of `room` in the shape rooms
/// held before [`administrator_grant`]: the owner's lasting Allow on the
/// room's resource, where it decides nothing an Editor's role does not
/// already deny. Anything else under the id (a Deny, an expiring grant) is
/// not one, so rewriting never widens it.
#[must_use]
pub fn is_legacy_administrator_grant(room: &str, grant: &Grant) -> bool {
    matches!(&grant.subject, Subject::Person(person) if grant.id == administrator_grant_id(person))
        && grant.node == room_resource(room)
        && grant.answer == Answer::Allow
        && grant.lifetime == Lifetime::UntilRevoked
}

/// Where comments live in a room, at any depth.
#[must_use]
pub fn comments(room: &str) -> TreePath {
    TreePath::room(room)
        .rest()
        .push(Level::Field, COMMENTS_FIELD)
}

/// Where reactions live in a room, at any depth.
#[must_use]
pub fn reactions(room: &str) -> TreePath {
    TreePath::room(room)
        .rest()
        .push(Level::Field, REACTIONS_FIELD)
}

/// Where moderation lives in a room, at any depth.
#[must_use]
pub fn moderation(room: &str) -> TreePath {
    TreePath::room(room)
        .rest()
        .push(Level::Field, MODERATION_FIELD)
}

/// What one who cannot read `room`'s grants (a page) asks the room to learn
/// its person's role, in order: may they write anywhere in it, moderate it,
/// and govern who holds which role. [`role_answered`] reads the answers.
#[must_use]
pub fn telling_nodes(room: &str) -> [TreePath; 3] {
    [
        TreePath::room(room),
        moderation(room).any(Level::Action),
        governance_action(room, governance::SET_ROLE),
    ]
}

/// The role the room's answers for [`telling_nodes`] describe. A Commenter
/// writes nowhere they ask, so reads as a Viewer.
#[must_use]
pub fn role_answered([writes, moderates, governs]: [bool; 3]) -> Role {
    if governs {
        Role::Owner
    } else if moderates {
        Role::Moderator
    } else if writes {
        Role::Editor
    } else {
        Role::Viewer
    }
}

/// The id of the grant that bans `person` from a room.
#[must_use]
pub fn block_grant_id(person: &str) -> GrantId {
    GrantId(format!("block:{person}"))
}

/// The grant that keeps `person` out of `room`: every kind denied on the
/// whole room, given by `granted_by`.
#[must_use]
pub fn block_grant(room: &str, person: &str, granted_by: Subject) -> Grant {
    Grant {
        id: block_grant_id(person),
        subject: Subject::Person(person.to_owned()),
        node: TreePath::room(room),
        kinds: KindSet::ALL,
        answer: Answer::Deny,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// The id of the grant that restricts `person` in a room.
#[must_use]
pub fn restriction_grant_id(person: &str) -> GrantId {
    GrantId(format!("restrict:{person}"))
}

/// The grant that stops `person` changing anything in `room` until
/// `until_ms` (Unix milliseconds, exclusive): undoable changes and outside
/// effects denied on the whole room, reading left alone. It lapses by itself,
/// so a temporary restriction needs nobody to come back and lift it.
#[must_use]
pub fn restriction_grant(room: &str, person: &str, until_ms: u64, granted_by: Subject) -> Grant {
    Grant {
        id: restriction_grant_id(person),
        subject: Subject::Person(person.to_owned()),
        node: TreePath::room(room),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Deny,
        lifetime: Lifetime::Until(until_ms),
        granted_by,
    }
}

/// The governance a Moderator may exercise: the membership actions and
/// restrictions, on members ranked below Moderator, and stopping or
/// restarting a bot, so a misbehaving bot can be halted without the
/// authority to widen it.
pub const MODERATOR_GOVERNANCE: [&str; 7] = [
    governance::REMOVE_MEMBER,
    governance::BAN,
    governance::UNBAN,
    governance::RESTRICT,
    governance::UNRESTRICT,
    governance::PAUSE_BOT,
    governance::RESUME_BOT,
];

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
/// | Commenter | Read: Allow; comments, reactions: Allow (Undoable) |
/// | Editor | Read, Undoable: Allow; Outside: Ask; governance, moderation: Deny |
/// | Moderator | as Editor; moderation: Allow; `remove-member`, `ban`, `unban`, `pause-bot`, `resume-bot`: Allow |
/// | Owner | everything: Allow |
///
/// A Moderator's membership actions reach only members ranked below
/// Moderator; [`crate::RoomPolicy`] enforces that, since it depends on the
/// target and not on the tree.
#[must_use]
pub fn role_rules(role: Role, room: &str) -> Vec<Rule> {
    let room_node = TreePath::room(room);
    match role {
        Role::Viewer => vec![Rule::new(room_node, KindSet::READ, Answer::Allow)],
        Role::Commenter => vec![
            Rule::new(room_node, KindSet::READ, Answer::Allow),
            Rule::new(comments(room), KindSet::UNDOABLE, Answer::Allow),
            Rule::new(reactions(room), KindSet::UNDOABLE, Answer::Allow),
        ],
        Role::Editor => editor_rules(room, Answer::Deny),
        Role::Moderator => {
            let mut rules = editor_rules(room, Answer::Allow);
            rules.extend(MODERATOR_GOVERNANCE.into_iter().map(|action| {
                Rule::new(
                    governance_action(room, action),
                    KindSet::UNDOABLE.union(KindSet::OUTSIDE),
                    Answer::Allow,
                )
            }));
            rules
        }
        Role::Owner => vec![Rule::new(room_node, KindSet::ALL, Answer::Allow)],
    }
}

fn editor_rules(room: &str, moderation_answer: Answer) -> Vec<Rule> {
    let room_node = TreePath::room(room);
    vec![
        Rule::new(room_node.clone(), KindSet::READ_UNDOABLE, Answer::Allow),
        Rule::new(room_node, KindSet::OUTSIDE, Answer::Ask),
        Rule::new(
            room_resource(room),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
        ),
        Rule::new(
            moderation(room),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            moderation_answer,
        ),
    ]
}

#[must_use]
pub fn role_preset(role: Role, room: &str) -> Preset {
    let description = match role {
        Role::Viewer => "Can read everything in the room.",
        Role::Commenter => "Can read everything and comment.",
        Role::Editor => {
            "Can read and make undoable changes; outside effects ask first; cannot moderate or govern the room."
        }
        Role::Moderator => {
            "Can do what an Editor can, moderate other people's content, and remove or ban members ranked below Moderator."
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

/// What an agent is in a room, decided in one place.
///
/// An agent is an ordinary collaborator acting for one person: it never holds
/// more than that person's role, never more than what its room admission
/// grants it, and never more than [`ROLE_CAP`](agent_policy::ROLE_CAP). Every host — the relay
/// governing a room, every member admitting its ops, the desktop broker
/// hosting a room itself — asks [`role`](agent_policy::role) rather than deciding on its own.
///
/// Four questions about agents are still open. Until they are decided each
/// keeps the answer the first agent-collaborator release gave it, and each
/// is changed here:
///
/// 1. Is an agent's role capped at Editor? [`ROLE_CAP`](agent_policy::ROLE_CAP): yes.
/// 2. May another person's agent join a room? [`OTHER_PEOPLES_AGENTS_JOIN`](agent_policy::OTHER_PEOPLES_AGENTS_JOIN):
///    no; a room's governor admits only its own person's agents.
/// 3. What does withdrawing an agent mean? [`WITHDRAWAL_IS_FINAL`](agent_policy::WITHDRAWAL_IS_FINAL): a
///    withdrawn agent device is never admitted again; granting it anew
///    needs a new agent key.
/// 4. Do members enforce the app scope of an agent's delegation? Not yet:
///    the agent's host does, and members enforce the delegation's rooms and
///    expiry only. Enforcing it at members needs the app on every op, which
///    the room's ops do not carry.
pub mod agent_policy {
    use crate::{Answer, KindSet, Role, Rule, Subject};

    /// The most an agent ever acts as, whoever it acts for. An agent never
    /// governs a room.
    pub const ROLE_CAP: Role = Role::Editor;

    /// Whether a room's governor admits an agent of a person other than its
    /// own (open question 2; see the module documentation).
    pub const OTHER_PEOPLES_AGENTS_JOIN: bool = false;

    /// Whether withdrawing an agent from a room is final for its device
    /// (open question 3; see the module documentation).
    pub const WITHDRAWAL_IS_FINAL: bool = true;

    /// The role an agent acts with when its person holds `person` and its
    /// room admission (its preset, or its delegation) grants it `granted`:
    /// the least of the two and [`ROLE_CAP`]. Never more than either.
    #[must_use]
    pub fn role(person: Role, granted: Role) -> Role {
        person.min(granted).min(ROLE_CAP)
    }

    /// The grants a person can hand to a task they start: a delegation never
    /// reaches further than the person launching it, and governance is the
    /// one thing it never carries at all. A task acts as the person who
    /// started it through this bound, so a grant the room holds for the task
    /// itself — or for the agent running it — decides for the task only as
    /// far as that person's own power does. However the room's governance
    /// verbs are granted, a task never governs its room.
    #[must_use]
    pub fn launch_bound(room: &str, delegation: Subject, person: &str) -> crate::Bound {
        let mut preset = crate::roles::role_preset(Role::Editor, room);
        // Every governance verb, named one by one: a grant naming the same
        // action for the task itself is no more specific than this Deny, and
        // a Deny wins at the same node, so the task never governs.
        preset
            .rules
            .extend(crate::roles::governance::ALL.into_iter().map(|action| {
                Rule::new(
                    crate::roles::governance_action(room, action),
                    KindSet::ALL,
                    Answer::Deny,
                )
            }));
        crate::Bound::delegation(delegation, &preset, person)
    }
}

/// The delegation a delegated agent `agent` of `person` carries in `room`:
/// the preset of [`agent_policy::ROLE_CAP`]. The agent acts with its person's
/// grants bounded by it, so it changes what its person may change, never more
/// than the cap allows, and never the room's governance.
#[must_use]
pub fn agent_delegation(room: &str, agent: &str, person: &str) -> crate::Bound {
    agent_policy::launch_bound(room, Subject::Agent(agent.to_owned()), person)
}

/// What an agent admitted to a room may do there on its own: the room-level
/// grants it receives. Never a human workspace role. A task a person starts
/// is theirs: the agent runs it as their delegate ([`agent_delegation`]),
/// so the preset never narrows what that person asked of it.
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
                grant(
                    ":reactions",
                    reactions(room),
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
