//! Presets: named grant sets. Roles are the built-in presets on a workspace; an
//! agent's default delegation and the foreign-device bound are presets too.

use serde::{Deserialize, Serialize};

use crate::{
    Answer, Authority, Context, Grant, GrantId, Grantee, KindSet, Level, Lifetime, Role, TreePath,
    Verdict,
};

/// The kind id of the workspace's own resource. Its actions are the workspace's
/// governance: membership, roles, grants and settings.
///
/// Records and grants already name this kind, in paths and in resources of
/// their own, so it keeps the spelling it was published under; the constant's
/// name is the job, not the spelling.
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

/// The workspace's governance actions.
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
    /// Change the workspace's settings.
    pub const SETTINGS: &str = "settings";
    /// Remove a member and keep them out until unbanned: a removal plus the
    /// `block:<person>` Deny.
    pub const BAN: &str = "ban";
    /// Lift a ban: revoke the person's `block:<person>` Deny.
    pub const UNBAN: &str = "unban";
    /// Stop a member changing anything until a time: the expiring
    /// `restrict:<person>` Deny. They keep reading the workspace.
    pub const RESTRICT: &str = "restrict";
    /// Lift a restriction before it lapses.
    pub const UNRESTRICT: &str = "unrestrict";
    /// Replace the workspace's history with a checkpoint.
    pub const COMPACT: &str = "compact";
    /// Remove an earlier change from the workspace's history.
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
    /// Choose the version of an app every member of the workspace runs: a
    /// registry release, or a developer's own signed build. Every member's
    /// desktop is to run it, and changes made with another version wait
    /// until their author runs this one.
    pub const INSTALL_APP: &str = "install-app";
    /// Start, end, restart or moderate one of the workspace's calls. A call is
    /// workspace governance (who may speak, who is denied), so no preset below
    /// the owner's holds it: the owner, and members the workspace grants it, run
    /// calls, and any member joins an active one.
    pub const CALL: &str = "call";

    pub const ALL: [&str; 21] = [
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
        INSTALL_APP,
        CALL,
    ];

    /// Actions that exercise authority over the workspace or others in it. The
    /// person's own grants must allow them: an Ask there is no authority the
    /// person's confirmation could supply. Pausing is not among them, so a
    /// moderator can stop a misbehaving bot without being able to widen it.
    pub const PRIVILEGED: [&str; 5] = [
        INSTALL_BOT,
        UNINSTALL_BOT,
        SET_BOT_APPEARANCE,
        ADOPT_BOT,
        INSTALL_APP,
    ];

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

/// The workspace's own resource: `workspace:<id>/kind:tilo.room`.
#[must_use]
pub fn workspace_resource(workspace: &str) -> TreePath {
    TreePath::workspace(workspace).kind(ROOM_KIND)
}

/// The node of assigning the named grant set `name` in `workspace`: a grant on
/// it lets someone assign that one set, and nothing else of the workspace.
#[must_use]
pub fn preset_assign_node(workspace: &str, name: &str) -> TreePath {
    workspace_resource(workspace)
        .field("presets")
        .item(name.trim().to_lowercase())
        .action("assign")
}

/// The verdict on `actor` assigning the saved grant set `set` in `workspace`:
/// a grant on the set's assign node allows that set alone, and otherwise the
/// workspace's grant action decides for every set, since assigning a set is
/// granting what it holds.
#[must_use]
pub fn assign_preset_verdict<'g>(
    actor: &Authority<'g>,
    workspace: &str,
    set: &str,
    cx: &Context<'_>,
) -> Verdict<'g> {
    let on_set = actor.decide(&preset_assign_node(workspace, set), GOVERNANCE_KIND, cx);
    if on_set.answer == Answer::Allow {
        return on_set;
    }
    actor.decide(
        &governance_action(workspace, governance::GRANT),
        GOVERNANCE_KIND,
        cx,
    )
}

/// The set and rule a grant id assigns, when it places one of the workspace's
/// saved grant sets: `preset:<mint>:<set>:<index>` ([`Preset::assign`]).
#[must_use]
pub fn preset_assigned(id: &GrantId) -> Option<(&str, usize)> {
    let rest = id.as_str().strip_prefix("preset:")?;
    let (_, rest) = rest.split_once(':')?;
    let (set, index) = rest.rsplit_once(':')?;
    Some((set, index.parse().ok()?))
}

#[must_use]
pub fn governance_action(workspace: &str, action: &str) -> TreePath {
    workspace_resource(workspace).action(action)
}

/// Every governance action of `workspace`: `workspace:<id>/kind:tilo.room/action:*`.
/// More specific than the workspace's resource, where the Editor's rule denies
/// governance, so a grant here decides each governance action whatever the
/// grantee's role says about the workspace's resource.
#[must_use]
pub fn governance_actions(workspace: &str) -> TreePath {
    workspace_resource(workspace).any(Level::Action)
}

/// The id of the grant that makes `person` an administrator of a workspace.
#[must_use]
pub fn administrator_grant_id(person: &str) -> GrantId {
    GrantId(format!("room-admin-v1:{person}"))
}

/// The grant that makes `person` an administrator of `workspace`, given by its
/// owner `granted_by`: every governance action allowed, on
/// [`governance_actions`].
///
/// An administrator stays a member with a role, an Editor as the workspace's
/// governance writes them, and that role still bounds them: an invite or a
/// role they hand out is never above it ([`crate::Refusal::AboveActorRole`]),
/// a membership action reaches only those ranked below it
/// ([`crate::Refusal::NotBelowActor`]), and a grant they give never exceeds
/// what they hold ([`crate::Refusal::AboveActorGrant`]). A ban or a live
/// restriction on them stops their governance with the rest of their
/// changes ([`crate::WorkspacePolicy::check_action`]).
///
/// Workspaces written before the grant took this node held it on
/// [`workspace_resource`], where the Editor's own Deny at the same node outranked
/// it; [`is_legacy_administrator_grant`] finds those so a member's governance
/// operation rewrites them.
#[must_use]
pub fn administrator_grant(workspace: &str, person: &str, granted_by: Grantee) -> Grant {
    Grant {
        id: administrator_grant_id(person),
        grantee: Grantee::person(person.to_owned()),
        node: governance_actions(workspace),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// The id of the grant that lets an administrator moderate `person`'s workspace.
#[must_use]
pub fn administrator_moderation_grant_id(person: &str) -> GrantId {
    GrantId(format!("room-admin-moderation-v1:{person}"))
}

/// The grant that lets `person`, an administrator of `workspace`, moderate it:
/// every moderation action allowed, as a Moderator may, given beside
/// [`administrator_grant`] by its giver. It sits one level below
/// [`moderation`], where an Editor's own role denies, so it decides every
/// moderation action whatever that role says.
#[must_use]
pub fn administrator_moderation_grant(workspace: &str, person: &str, granted_by: Grantee) -> Grant {
    Grant {
        id: administrator_moderation_grant_id(person),
        grantee: Grantee::person(person.to_owned()),
        node: moderation(workspace).any(Level::Action),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// Whether `grant` is an administrator's grant of `workspace` in the shape workspaces
/// held before [`administrator_grant`]: the owner's lasting Allow on the
/// workspace's resource, where it decides nothing an Editor's role does not
/// already deny. Anything else under the id (a Deny, an expiring grant) is
/// not one, so rewriting never widens it.
#[must_use]
pub fn is_legacy_administrator_grant(workspace: &str, grant: &Grant) -> bool {
    matches!(&grant.grantee, Grantee::Member(crate::MemberId::Person(person)) if grant.id == administrator_grant_id(person))
        && grant.node == workspace_resource(workspace)
        && grant.answer == Answer::Allow
        && grant.lifetime == Lifetime::UntilRevoked
}

/// Where comments live in a workspace, at any depth.
#[must_use]
pub fn comments(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .rest()
        .push(Level::Field, COMMENTS_FIELD)
}

/// Where reactions live in a workspace, at any depth.
#[must_use]
pub fn reactions(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .rest()
        .push(Level::Field, REACTIONS_FIELD)
}

/// Where moderation lives in a workspace, at any depth.
#[must_use]
pub fn moderation(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .rest()
        .push(Level::Field, MODERATION_FIELD)
}

/// What one who cannot read `workspace`'s grants (a page) asks the workspace to learn
/// its person's role, in order: may they write anywhere in it, moderate it,
/// and govern who holds which role. [`role_answered`] reads the answers.
#[must_use]
pub fn telling_nodes(workspace: &str) -> [TreePath; 3] {
    [
        TreePath::workspace(workspace),
        moderation(workspace).any(Level::Action),
        governance_action(workspace, governance::SET_ROLE),
    ]
}

/// The role the workspace's answers for [`telling_nodes`] describe. A Commenter
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

/// The id of the grant that bans `person` from a workspace.
#[must_use]
pub fn block_grant_id(person: &str) -> GrantId {
    GrantId(format!("block:{person}"))
}

/// The grant that keeps `person` out of `workspace`: every kind denied on the
/// whole workspace, given by `granted_by`.
#[must_use]
pub fn block_grant(workspace: &str, person: &str, granted_by: Grantee) -> Grant {
    Grant {
        id: block_grant_id(person),
        grantee: Grantee::person(person.to_owned()),
        node: TreePath::workspace(workspace),
        kinds: KindSet::ALL,
        answer: Answer::Deny,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// The id of the grant that restricts `person` in a workspace.
#[must_use]
pub fn restriction_grant_id(person: &str) -> GrantId {
    GrantId(format!("restrict:{person}"))
}

/// The grant that stops `person` changing anything in `workspace` until
/// `until_ms` (Unix milliseconds, exclusive): undoable changes and outside
/// effects denied on the whole workspace, reading left alone. It lapses by itself,
/// so a temporary restriction needs nobody to come back and lift it.
#[must_use]
pub fn restriction_grant(
    workspace: &str,
    person: &str,
    until_ms: u64,
    granted_by: Grantee,
) -> Grant {
    Grant {
        id: restriction_grant_id(person),
        grantee: Grantee::person(person.to_owned()),
        node: TreePath::workspace(workspace),
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

/// One rule of a preset: a grant without its grantee.
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

    /// The preset's grants for `grantee`. Ids are `<prefix>:<index>`.
    #[must_use]
    pub fn assign(
        &self,
        prefix: &str,
        grantee: &Grantee,
        granted_by: &Grantee,
        lifetime: &Lifetime,
    ) -> Vec<Grant> {
        self.rules
            .iter()
            .enumerate()
            .map(|(index, rule)| Grant {
                id: GrantId(format!("{prefix}:{index}")),
                grantee: grantee.clone(),
                node: rule.node.clone(),
                kinds: rule.kinds,
                answer: rule.answer,
                lifetime: lifetime.clone(),
                granted_by: granted_by.clone(),
            })
            .collect()
    }
}

/// The rules a role carries in `workspace`.
///
/// | Role | Grants on the workspace |
/// |---|---|
/// | Viewer | Read: Allow |
/// | Commenter | Read: Allow; comments, reactions: Allow (Undoable) |
/// | Editor | Read, Undoable: Allow; Outside: Ask; governance, moderation: Deny |
/// | Moderator | as Editor; moderation: Allow; `remove-member`, `ban`, `unban`, `pause-bot`, `resume-bot`: Allow |
/// | Owner | everything: Allow |
///
/// A Moderator's membership actions reach only members ranked below
/// Moderator; [`crate::WorkspacePolicy`] enforces that, since it depends on the
/// target and not on the tree.
#[must_use]
pub fn role_rules(role: Role, workspace: &str) -> Vec<Rule> {
    let workspace_node = TreePath::workspace(workspace);
    match role {
        Role::Viewer => vec![Rule::new(workspace_node, KindSet::READ, Answer::Allow)],
        Role::Commenter => vec![
            Rule::new(workspace_node, KindSet::READ, Answer::Allow),
            Rule::new(comments(workspace), KindSet::UNDOABLE, Answer::Allow),
            Rule::new(reactions(workspace), KindSet::UNDOABLE, Answer::Allow),
        ],
        Role::Editor => editor_rules(workspace, Answer::Deny),
        Role::Moderator => {
            let mut rules = editor_rules(workspace, Answer::Allow);
            rules.extend(MODERATOR_GOVERNANCE.into_iter().map(|action| {
                Rule::new(
                    governance_action(workspace, action),
                    KindSet::UNDOABLE.union(KindSet::OUTSIDE),
                    Answer::Allow,
                )
            }));
            rules
        }
        Role::Owner => vec![Rule::new(workspace_node, KindSet::ALL, Answer::Allow)],
    }
}

fn editor_rules(workspace: &str, moderation_answer: Answer) -> Vec<Rule> {
    let workspace_node = TreePath::workspace(workspace);
    vec![
        Rule::new(
            workspace_node.clone(),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
        ),
        Rule::new(workspace_node, KindSet::OUTSIDE, Answer::Ask),
        Rule::new(
            workspace_resource(workspace),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            Answer::Deny,
        ),
        Rule::new(
            moderation(workspace),
            KindSet::UNDOABLE.union(KindSet::OUTSIDE),
            moderation_answer,
        ),
    ]
}

#[must_use]
pub fn role_preset(role: Role, workspace: &str) -> Preset {
    let description = match role {
        Role::Viewer => "Can read everything in the workspace.",
        Role::Commenter => "Can read everything and comment.",
        Role::Editor => {
            "Can read and make undoable changes, except the workspace's own settings; outside effects ask first."
        }
        Role::Moderator => {
            "Can do what an Editor can, moderate other people's content, and remove or ban members ranked below Moderator."
        }
        Role::Owner => "Can do everything, including governing the workspace.",
    };
    Preset::new(role.name(), description, role_rules(role, workspace))
}

/// Every role's grants for a new workspace, given by its owner. Ids are
/// `role:<role>:<index>`.
#[must_use]
pub fn workspace_role_grants(workspace: &str, granted_by: &Grantee) -> Vec<Grant> {
    Role::ALL
        .into_iter()
        .flat_map(|role| {
            role_preset(role, workspace).assign(
                &role_grant_prefix(role),
                &Grantee::role(role),
                granted_by,
                &Lifetime::UntilRevoked,
            )
        })
        .chain([
            session_app(workspace, granted_by.clone()),
            device_panel(workspace, granted_by.clone()),
            files_app(workspace, granted_by.clone()),
        ])
        .collect()
}

/// The id prefix of `role`'s built-in grants: `role:<role>`.
fn role_grant_prefix(role: Role) -> String {
    format!("role:{}", role.name().to_ascii_lowercase())
}

/// Whether `grant` is one of a built-in role's grants: given to the role
/// itself under a `role:<role>:<index>` id. Those are the code's, from
/// [`role_rules`]; a workspace's own adjustments to a role are grants under other
/// ids and custom presets assign `preset:` ids, so neither is one.
#[must_use]
pub fn is_role_grant(grant: &Grant) -> bool {
    let Grantee::Role(role) = grant.grantee else {
        return false;
    };
    grant
        .id
        .as_str()
        .strip_prefix(role_grant_prefix(role).as_str())
        .and_then(|rest| rest.strip_prefix(':'))
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
}

/// `grants` with each built-in role's grants as [`role_rules`] defines them
/// now, so a role's rules have one definition and reach workspaces written before
/// they changed.
///
/// A role the workspace holds any built-in grant of gets exactly the role's
/// current grants, on the workspace that grant names: a stored one the role still
/// has is replaced by the current one in its place, keeping who gave it; one
/// the role no longer has is dropped; one the role gained follows the role's
/// last stored grant, given by whoever gave its first. A role the workspace holds
/// none of stays absent. Every other grant stays as it is, in order. A workspace
/// already current comes back unchanged.
#[must_use]
pub fn current_role_grants(grants: Vec<Grant>) -> Vec<Grant> {
    struct Held {
        role: Role,
        current: Vec<Grant>,
        placed: Vec<bool>,
        last: usize,
    }
    let mut held: Vec<Held> = Vec::new();
    let mut out: Vec<Option<Grant>> = Vec::with_capacity(grants.len());
    for grant in grants {
        if !is_role_grant(&grant) {
            out.push(Some(grant));
            continue;
        }
        let Grantee::Role(role) = grant.grantee else {
            continue;
        };
        if !held.iter().any(|entry| entry.role == role) {
            let Some(workspace) = grant.node.workspace_id() else {
                out.push(Some(grant));
                continue;
            };
            let current = role_preset(role, workspace).assign(
                &role_grant_prefix(role),
                &grant.grantee,
                &grant.granted_by,
                &Lifetime::UntilRevoked,
            );
            held.push(Held {
                role,
                placed: vec![false; current.len()],
                current,
                last: 0,
            });
        }
        let Some(entry) = held.iter_mut().find(|entry| entry.role == role) else {
            continue;
        };
        entry.last = out.len();
        let slot = entry
            .current
            .iter()
            .zip(entry.placed.iter_mut())
            .find(|(current, placed)| current.id == grant.id && !**placed);
        match slot {
            Some((current, placed)) => {
                *placed = true;
                out.push(Some(Grant {
                    granted_by: grant.granted_by,
                    ..current.clone()
                }));
            }
            None => out.push(None),
        }
    }
    let mut gained: Vec<(usize, Vec<Grant>)> = held
        .into_iter()
        .map(|entry| {
            let missing = entry
                .current
                .into_iter()
                .zip(entry.placed)
                .filter(|(_, placed)| !placed)
                .map(|(grant, _)| grant)
                .collect();
            (entry.last, missing)
        })
        .collect();
    gained.sort_by_key(|(last, _)| std::cmp::Reverse(*last));
    for (last, missing) in gained {
        let at = last.saturating_add(1).min(out.len());
        out.splice(at..at, missing.into_iter().map(Some));
    }
    out.into_iter().flatten().collect()
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

/// What an agent is in a workspace, decided in one place.
///
/// An agent acts for one person, inside a turn that person's message caused
/// ([`crate::turn::authority`]): it never holds more than that person's role,
/// and never more than [`ROLE_CAP`](agent_policy::ROLE_CAP). Every host — the
/// relay governing a workspace, every member admitting its ops, the desktop
/// broker hosting a workspace itself — asks [`role`](agent_policy::role) rather
/// than deciding on its own. A workspace's governor admits only its own
/// person's agents.
///
/// Two questions about agents are still open. Until they are decided each
/// keeps the answer the first agent-collaborator release gave it, and each
/// is changed here:
///
/// 1. Is an agent's role capped at Editor? [`ROLE_CAP`](agent_policy::ROLE_CAP): yes.
/// 2. What does withdrawing an agent mean? [`WITHDRAWAL_IS_FINAL`](agent_policy::WITHDRAWAL_IS_FINAL): a
///    withdrawn agent device is never admitted again; granting it anew
///    needs a new agent key.
///
/// A member still admits an agent's ops by its delegation's workspaces and
/// expiry only: the app scope of its record is enforced by the turn's door.
pub mod agent_policy {
    use crate::Role;

    /// The most an agent ever acts as, whoever it acts for. An agent never
    /// governs a workspace.
    pub const ROLE_CAP: Role = Role::Editor;

    /// Whether withdrawing an agent from a workspace is final for its device
    /// (open question 2; see the module documentation).
    pub const WITHDRAWAL_IS_FINAL: bool = true;

    /// The role an agent acts with when its person holds `person` and its
    /// delegation grants it `granted`: the least of the two and [`ROLE_CAP`].
    /// Never more than either.
    #[must_use]
    pub fn role(person: Role, granted: Role) -> Role {
        person.min(granted).min(ROLE_CAP)
    }
}

/// The authority an agent's turn carries in `workspace`: the preset of
/// [`agent_policy::ROLE_CAP`], narrowed by the bound the agent's record keeps
/// (`bound`; empty follows the default, which is the cap's own). The agent acts
/// with its person's grants bounded by them, so it changes what its person may
/// change, never more than the cap allows, never more than its record bounds it
/// to, and never the workspace's governance.
///
/// This is the only place an agent's authority is made: a door or a receiver
/// hands it the person whose message caused the turn, and the bound it holds
/// for the agent's record ([`crate::turn::authority`]).
#[must_use]
pub fn agent_grants(workspace: &str, agent: &str, person: &str, bound: &[Rule]) -> Vec<Grant> {
    let grantee = Grantee::agent(agent.to_owned());
    let granted_by = Grantee::person(person.to_owned());
    let prefix = format!("turn:agent:{agent}");
    let mut grants = role_preset(agent_policy::ROLE_CAP, workspace).assign(
        &prefix,
        &grantee,
        &granted_by,
        &Lifetime::UntilRevoked,
    );
    grants.extend(bound.iter().enumerate().map(|(index, rule)| Grant {
        id: GrantId(format!("{prefix}:bound:{index}")),
        grantee: grantee.clone(),
        node: rule.node.clone(),
        kinds: rule.kinds,
        answer: rule.answer,
        lifetime: Lifetime::UntilRevoked,
        granted_by: granted_by.clone(),
    }));
    grants
}

/// The grant making application `app` an author of its own records in
/// `workspace`: it reads and changes what it keeps there as the person using it,
/// and reaches nothing outside it. Another app's actions in the workspace stay
/// behind the workspace's own grants and the person's consent. Given when a
/// person who may change the workspace opens the app.
#[must_use]
pub fn app_authorship(workspace: &str, app: &str, granted_by: Grantee) -> Grant {
    Grant::new(
        format!("app-author:{app}"),
        Grantee::app(app.to_owned()),
        TreePath::workspace(workspace).app(app),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        granted_by,
    )
}

/// The app a person's desktop session publishes its own functions as (the
/// shell's actions: windows, workspaces, its settings), on their device.
pub const SESSION_APP: &str = "shell";

/// The id of the grant [`session_app`] makes.
pub const SESSION_APP_GRANT: &str = "session-app";

/// Whether `grant` is the one [`session_app`] makes: part of the workspace's
/// make-up, never one a person gave and may take back.
#[must_use]
pub fn is_session_app(grant: &Grant) -> bool {
    grant.id.as_str() == SESSION_APP_GRANT
        && matches!(&grant.grantee, Grantee::App(app) if app == SESSION_APP)
}

/// The grant making the desktop session's own app, [`SESSION_APP`], one of
/// the person's own apps in `workspace`: whatever the person may do there, it may
/// do for them. It never reaches past the person it acts for, whose authority
/// bounds every delegation.
#[must_use]
pub fn session_app(workspace: &str, granted_by: Grantee) -> Grant {
    Grant::new(
        SESSION_APP_GRANT,
        Grantee::app(SESSION_APP.to_owned()),
        TreePath::workspace(workspace),
        KindSet::ALL,
        Answer::Allow,
        granted_by,
    )
}

/// The id of the grant [`device_panel`] makes.
pub const DEVICE_PANEL_GRANT: &str = "device-panel";

/// Whether `grant` is the one [`device_panel`] makes: part of the workspace's
/// make-up, like [`session_app`]'s.
#[must_use]
pub fn is_device_panel(grant: &Grant) -> bool {
    grant.id.as_str() == DEVICE_PANEL_GRANT
        && matches!(&grant.grantee, Grantee::App(app) if app == DEVICE_PANEL)
}

/// Whether `grant` is one the code defines and every read of a stored policy
/// restores ([`crate::WorkspacePolicy::with_current_roles`]): a built-in
/// role's, or part of the room's make-up ([`session_app`], [`device_panel`],
/// [`files_app`]). Whatever narrows a policy and reads it back narrows these
/// again.
#[must_use]
pub fn is_code_defined(grant: &Grant) -> bool {
    is_role_grant(grant) || is_session_app(grant) || is_device_panel(grant) || is_files_app(grant)
}

/// Whether `grant` is one of the room's make-up, never one a person gave and
/// may take back: [`session_app`], [`device_panel`] or [`files_app`].
#[must_use]
pub fn is_make_up(grant: &Grant) -> bool {
    is_session_app(grant) || is_device_panel(grant) || is_files_app(grant)
}

/// The app a workspace's files are the record of: the tree every device
/// holding the workspace mounts or mirrors, published as this app's record.
pub const FILES_APP: &str = "so.tilo.file-explorer";

/// The id of the grant [`files_app`] makes.
pub const FILES_APP_GRANT: &str = "files-app";

/// Whether `grant` is the one [`files_app`] makes: part of the workspace's
/// make-up, like [`session_app`]'s.
#[must_use]
pub fn is_files_app(grant: &Grant) -> bool {
    grant.id.as_str() == FILES_APP_GRANT
        && matches!(&grant.grantee, Grantee::App(app) if app == FILES_APP)
}

/// The grant making [`FILES_APP`] an author of `workspace`'s files: every
/// workspace has files, kept as that app's record, so whoever the workspace
/// lets change its content changes its files through it, and the first of
/// them to hold any starts the record. Whether a member may write stays
/// their role's: a viewer still writes nothing.
#[must_use]
pub fn files_app(workspace: &str, granted_by: Grantee) -> Grant {
    Grant::new(
        FILES_APP_GRANT,
        Grantee::app(FILES_APP.to_owned()),
        TreePath::workspace(workspace).app(FILES_APP),
        KindSet::READ_UNDOABLE,
        Answer::Allow,
        granted_by,
    )
}

/// The app a person's device shows the session's own state in: quick
/// settings, which is a panel of the session's rather than an app of its own.
pub const DEVICE_PANEL: &str = "so.tilo.quick-settings";

/// The grant letting the device's panel, [`DEVICE_PANEL`], read what the
/// session publishes for it in `workspace`: the readings of its own resources
/// that are nobody else's to read, such as what the person copied. It reads
/// them, and nothing about them: changing the device stays the person's.
#[must_use]
pub fn device_panel(workspace: &str, granted_by: Grantee) -> Grant {
    Grant::new(
        DEVICE_PANEL_GRANT,
        Grantee::app(DEVICE_PANEL.to_owned()),
        TreePath::workspace(workspace)
            .app(SESSION_APP)
            .kind(SESSION_KIND),
        KindSet::READ,
        Answer::Allow,
        granted_by,
    )
}

/// The kind the session publishes its own resources under: the compositor's
/// state, the clipboard, the lock.
pub const SESSION_KIND: &str = "session";
