//! Presets: named grant sets. Roles are the built-in presets on a workspace; an
//! agent's default delegation and the foreign-device bound are presets too.

use serde::{Deserialize, Serialize};

use crate::{
    Answer, Authority, Context, Grant, GrantId, Grantee, KindSet, Level, Lifetime, Role, TreePath,
    Verdict,
};

/// Whether `app`'s records of `kind` are each one person's, as its manifest declares
/// (`[resources] private`): what they are writing and have not sent. Every role is denied them
/// ([`role_rules`]) and the workspace's ownership does not reach them; each person reads and
/// changes their own ([`own_records`]), and an owner may give others more with a grant like any
/// other.
#[must_use]
pub fn has_member_reader(app: &str, kind: &str) -> bool {
    app_fleet::private_kinds(app).any(|declared| declared == kind)
}

/// The id of `person`'s record of `kind`: a version-8 uuid derived from both, the same on every
/// device of theirs, naming neither. `None` when `person` is not a person id (the lowercase hex
/// of their user root key).
#[must_use]
pub fn member_record(kind: &str, person: &str) -> Option<String> {
    let mut root = [0; 32];
    hex::decode_to_slice(person, &mut root).ok()?;
    let mut hasher = blake3::Hasher::new_derive_key("tilo own record v1");
    hasher.update(&root);
    hasher.update(kind.as_bytes());
    let mut bytes = [0; 16];
    for (byte, derived) in bytes.iter_mut().zip(hasher.finalize().as_bytes()) {
        *byte = *derived;
    }
    Some(format!(
        "uuid:{}",
        uuid::Builder::from_custom_bytes(bytes).into_uuid()
    ))
}

/// The id of the workspace of the person whose root is `person`: the same on each of their
/// devices, so none has to be told it. It is an ordinary workspace whose only member is the
/// person: it holds what is theirs alone, their servers, devices, workspace list and the settings
/// that follow them, and decides the changes of those records.
#[must_use]
pub fn person_workspace_id(person: &[u8; 32]) -> String {
    // The domain names the workspace's first use, the person's servers; the
    // ids of the workspaces already founded follow from it.
    let hash = blake3::Hasher::new()
        .update(b"tilo admin workspace v1\0")
        .update(person)
        .finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&hash.as_bytes()[..16]);
    uuid::Builder::from_random_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// [`person_workspace_id`] of the person `person` names (the lowercase hex of their user root
/// key); `None` when it is not a person id.
#[must_use]
pub fn workspace_of_person(person: &str) -> Option<String> {
    let mut root = [0; 32];
    hex::decode_to_slice(person, &mut root).ok()?;
    Some(person_workspace_id(&root))
}

/// The derived record `record` belongs to: itself, or the record a part, chunk or child of it
/// names. Only [`member_record`] makes version-8 ids.
fn derived_record_in(record: &str) -> Option<&str> {
    record.match_indices("uuid:").find_map(|(at, _)| {
        let id = record.get(at..)?.get(..41)?;
        let uuid = uuid::Uuid::try_parse(id.strip_prefix("uuid:")?).ok()?;
        (uuid.get_version_num() == 8).then_some(id)
    })
}

/// Whether `app`'s `record` is one person's private record, or a part, chunk or child of one: only
/// its person reaches it ([`own_records`]), or a grant, never the workspace's ownership.
#[must_use]
pub fn is_private_record(app: &str, record: &str) -> bool {
    app_fleet::private_kinds(app).next().is_some() && derived_record_in(record).is_some()
}

/// Every private record kind of `workspace` ([`has_member_reader`]), which no role reads or changes.
fn private_record_rules(workspace: &str) -> impl Iterator<Item = Rule> + '_ {
    app_fleet::every_private_kind().map(move |(app, kind)| {
        Rule::new(
            TreePath::workspace(workspace).app(app).kind(kind),
            KindSet::READ_UNDOABLE,
            Answer::Deny,
        )
    })
}

/// Whether `node`, as [`decided_node`] reads it, is in one person's private record, which the
/// workspace's ownership does not reach.
pub(crate) fn in_private_record(node: &TreePath) -> bool {
    node.id_at(Level::App)
        .zip(node.id_at(Level::Kind))
        .is_some_and(|(app, kind)| has_member_reader(app, kind))
        && node.id_at(Level::Resource).is_some()
}

/// Whether `node` is a device's settings resource: the settings app's
/// `devices/<device>/settings/<name>` (`settings_store::placement`), kept in that device's person's
/// own workspace. A device's settings are its owner's, so neither the workspace's ownership nor
/// its role presets reach another person's device; only the person whose own workspace this is,
/// or someone they granted there, writes them.
#[must_use]
pub fn is_device_settings(node: &TreePath) -> bool {
    node.id_at(Level::App) == Some(tilo_app_id::apps::SETTINGS)
        && node.id_at(Level::Resource).is_some_and(|resource| {
            resource
                .strip_prefix("devices/")
                .and_then(|device| device.split_once("/settings/"))
                .is_some_and(|(device, name)| !device.is_empty() && !name.is_empty())
        })
}

/// Whether `node` is in a record no one reads, as its declaration says
/// ([`app_fleet::Readers::NoOne`]).
pub(crate) fn in_unread_record(node: &TreePath) -> bool {
    node.id_at(Level::App)
        .zip(node.id_at(Level::Resource))
        .and_then(|(app, record)| app_fleet::declared_record(app, record))
        .is_some_and(|kept| kept.readers == app_fleet::Readers::NoOne)
}

/// The kind id of the workspace's own resource. Its actions are the workspace's
/// governance: membership, roles, grants and settings.
pub const WORKSPACE_KIND: &str = "tilo.workspace";

/// The field holding comments, wherever a model keeps them. The Commenter
/// preset covers it at any depth, so field edits and verbs published on it are
/// what a commenter may do.
pub const COMMENTS_FIELD: &str = "comments";

/// The app that holds what the workspace's people say about its resources: a comment on a task, a
/// note or a mail is one of its records, made from what it is about, not a field of that
/// resource. Writing there is commenting, so the Commenter preset covers it as it covers
/// [`COMMENTS_FIELD`]; who may edit or delete a comment once it is said is the app's own rule
/// (only its author).
pub const DISCUSSION_APP: &str = "so.tilo.lang.discussion";

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
    /// Let someone in, at most at the role the one letting them in holds: directly, or by an
    /// invitation, which is letting them in put off until they redeem it.
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
    /// Rename the workspace. Its name is governance every list of it reads,
    /// so the owner and the moderators decide it, as they decide whether it
    /// exists: a name is what people find it by.
    pub const RENAME: &str = "rename";
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
    /// Hand the workspace to another person, who becomes its owner. The owner
    /// alone does it ([`crate::Refusal::OwnershipByOwner`]); it is no action a
    /// grant gives, so it is not among [`ALL`].
    pub const TRANSFER_OWNERSHIP: &str = "transfer-ownership";
    /// Come into the workspace by accepting an invitation: the change the
    /// person's own device writes, which the invitation authorizes. Nobody
    /// is granted it, so it is not among [`ALL`]; it names the change for
    /// whoever reads the workspace's members.
    pub const JOIN: &str = "join";

    pub const ALL: [&str; 15] = [
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
        RENAME,
        COMPACT,
        REDACT,
        INSTALL_APP,
        CALL,
    ];

    /// Whether `action` exercises authority over the workspace or others in it. The
    /// person's own grants must allow it: an Ask there is no authority the
    /// person's confirmation could supply.
    #[must_use]
    pub fn is_privileged(action: &str) -> bool {
        action == INSTALL_APP
    }
}

/// The kind of every governance action. Changing who may do what reaches
/// beyond the document — it decides what other people see and do — so an
/// agent's default asks before it, and the wire never lets one through on an
/// answer that would need asking.
pub const GOVERNANCE_KIND: crate::ActionKind = crate::ActionKind::Outside;

/// The workspace's own resource: `workspace:<id>/kind:tilo.workspace`.
#[must_use]
pub fn workspace_resource(workspace: &str) -> TreePath {
    TreePath::workspace(workspace).kind(WORKSPACE_KIND)
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

/// Every governance action of `workspace`: `workspace:<id>/kind:tilo.workspace/action:*`.
/// More specific than the workspace's resource, where the Editor's rule denies
/// governance, so a grant here decides each governance action whatever the
/// grantee's role says about the workspace's resource.
#[must_use]
pub fn governance_actions(workspace: &str) -> TreePath {
    workspace_resource(workspace).any(Level::Action)
}

/// What the ids of the grants that make someone an administrator start
/// with: the writing and the reading of each id share one spelling.
const ADMINISTRATOR_GRANT_PREFIX: &str = "workspace-admin-v1:";
const ADMINISTRATOR_MODERATION_GRANT_PREFIX: &str = "workspace-admin-moderation-v1:";

/// The id of the grant that makes `person` an administrator of a workspace.
#[must_use]
pub fn administrator_grant_id(person: &str) -> GrantId {
    GrantId(format!("{ADMINISTRATOR_GRANT_PREFIX}{person}"))
}

/// The grant that makes `person` an administrator of `workspace`, given by its
/// owner `granted_by`: every governance action allowed, on
/// [`governance_actions`].
///
/// An administrator stays a member with a role, an Editor as the workspace's
/// governance writes them, for what they do with the workspace's content.
/// Administering it, they are the owner's equal in all but the owner's place:
/// they hand out every role a workspace hands out, and a membership action
/// reaches every member who is not an administrator; other administrators
/// are the owner's to unmake or remove
/// ([`crate::Refusal::Administrators`]). A grant they give never exceeds
/// what they hold ([`crate::Refusal::AboveActorGrant`]). A ban or a live
/// restriction on them stops their governance with the rest of their
/// changes ([`crate::WorkspacePolicy::check_action`]).
///
/// Workspaces written before this grant took [`governance_actions`] held it on
/// [`workspace_resource`], where the Editor's own Deny at the same node
/// outranked it; [`is_legacy_administrator_grant`] finds those and
/// [`current_administrator_grant`] reads them as this grant.
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

/// Whether `grant` is an administrator's grant of `workspace` in the shape
/// workspaces held before [`administrator_grant`]: the owner's lasting Allow on
/// the workspace's resource, where the Editor's own Deny at the same node
/// outranked it. Anything else under the id (a Deny, an expiring grant) is not
/// one, so reading it as [`administrator_grant`] never widens it.
#[must_use]
pub fn is_legacy_administrator_grant(workspace: &str, grant: &Grant) -> bool {
    grant
        .grantee
        .person_id()
        .is_some_and(|person| grant.id == administrator_grant_id(person))
        && grant.node == workspace_resource(workspace)
        && grant.answer == Answer::Allow
        && grant.lifetime == Lifetime::UntilRevoked
}

/// `grant` as the workspace's policy holds it: an administrator's grant of the
/// shape workspaces held before [`administrator_grant`] becomes that grant,
/// keeping its id and giver, so whom it makes an administrator is one on every
/// device that reads the change ([`is_legacy_administrator_grant`]). Every
/// other grant is itself.
#[must_use]
pub fn current_administrator_grant(workspace: &str, grant: Grant) -> Grant {
    let person = grant.grantee.person_id().map(str::to_owned);
    match person {
        Some(person) if is_legacy_administrator_grant(workspace, &grant) => {
            administrator_grant(workspace, &person, grant.granted_by)
        }
        _ => grant,
    }
}

/// The id of the grant that lets an administrator moderate `person`'s workspace.
#[must_use]
pub fn administrator_moderation_grant_id(person: &str) -> GrantId {
    GrantId(format!("{ADMINISTRATOR_MODERATION_GRANT_PREFIX}{person}"))
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

/// Whose administrator grant `id` names, when it is one
/// ([`administrator_grant_id`]); the moderation grant beside it is not.
#[must_use]
pub fn administrator_grant_person(id: &GrantId) -> Option<&str> {
    id.as_str().strip_prefix(ADMINISTRATOR_GRANT_PREFIX)
}

/// Whether `id` is one of the two grants that make someone an administrator
/// ([`administrator_grant`] and [`administrator_moderation_grant`]), and whose:
/// placing either is making an administrator, taking either back unmaking one.
#[must_use]
pub fn administrator_named(id: &GrantId) -> Option<&str> {
    let id = id.as_str();
    id.strip_prefix(ADMINISTRATOR_GRANT_PREFIX)
        .or_else(|| id.strip_prefix(ADMINISTRATOR_MODERATION_GRANT_PREFIX))
}

/// The node of making and unmaking a workspace's administrators. It is not
/// one of the actions a member runs on its own ([`governance::ALL`]): it is
/// what deciding [`crate::WorkspacePolicy::may_make_administrators`] asks.
/// The owner holds it, and every administrator holds it through their grant
/// on [`governance_actions`].
#[must_use]
pub fn administrators_node(workspace: &str) -> TreePath {
    governance_action(workspace, "administer")
}

/// Where comments live in a workspace, at any depth.
#[must_use]
pub fn comments(workspace: &str) -> TreePath {
    TreePath::workspace(workspace)
        .rest()
        .push(Level::Field, COMMENTS_FIELD)
}

/// Where the workspace's discussion lives: the comments [`DISCUSSION_APP`] keeps.
#[must_use]
pub fn discussion(workspace: &str) -> TreePath {
    TreePath::workspace(workspace).app(DISCUSSION_APP)
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
pub fn role_answered([writes, moderates, administers]: [bool; 3]) -> Role {
    if administers {
        Role::Owner
    } else if moderates {
        Role::Moderator
    } else if writes {
        Role::Editor
    } else {
        Role::Viewer
    }
}

/// The records each member keeps their own of, which every member reads and only its person
/// changes, as their kinds' declarations say ([`app_fleet::WorkspaceDocument::each_person`]), and
/// the node of each kind in `workspace`.
fn each_persons(
    workspace: &str,
) -> impl Iterator<Item = (&'static app_fleet::WorkspaceDocument, TreePath)> {
    app_fleet::WORKSPACE_DOCUMENTS
        .iter()
        .filter(|kept| kept.each_person.is_some())
        .map(move |kept| {
            (
                kept,
                TreePath::workspace(workspace).app(kept.app).kind(kept.kind),
            )
        })
}

/// `person`'s own records in `workspace` ([`each_persons`]).
fn persons_own(workspace: &str, person: &str) -> impl Iterator<Item = TreePath> {
    each_persons(workspace)
        .filter_map(move |(kept, node)| Some(node.resource(kept.record_of(person)?)))
}

/// `node` as a decision reads it: a node in a member's own record ([`each_persons`]) is of its
/// kind, and a node in a person's private record is of its kind and in the record
/// itself rather than a part of it ([`has_member_reader`]), whichever kind, if any, its caller
/// named, so the rules on them decide it wherever it is asked from.
#[must_use]
pub fn decided_node(node: &TreePath) -> std::borrow::Cow<'_, TreePath> {
    use crate::Segment;
    let segments = node.segments();
    let app = segments.iter().find_map(|segment| match segment {
        Segment::Name(Level::App, app) => Some(app.as_str()),
        _ => None,
    });
    let record = segments.iter().find_map(|segment| match segment {
        Segment::Name(Level::Resource, record) => Some(record.as_str()),
        _ => None,
    });
    let (Some(app), Some(record)) = (app, record) else {
        return std::borrow::Cow::Borrowed(node);
    };
    let decided_as = if let Some((kept, _)) = app_fleet::person_record(app, record) {
        Some((kept.kind, record))
    } else {
        let named = node.id_at(Level::Kind);
        let mut kinds = app_fleet::private_kinds(app).peekable();
        let first = kinds.peek().copied();
        kinds
            .find(|kind| Some(*kind) == named)
            .or(first)
            .zip(derived_record_in(record))
    };
    let Some((kind, as_record)) = decided_as else {
        return std::borrow::Cow::Borrowed(node);
    };
    if node.id_at(Level::Kind) == Some(kind) && as_record == record {
        return std::borrow::Cow::Borrowed(node);
    }
    let mut decided = TreePath::default();
    for segment in segments {
        match segment {
            Segment::Name(Level::Kind, _) => {}
            Segment::Name(Level::Resource, _) => {
                decided.push_name(Level::Kind, kind);
                decided.push_name(Level::Resource, as_record);
            }
            Segment::Name(level, id) => decided.push_name(*level, id.clone()),
            Segment::Any(level) => decided = decided.any(*level),
            Segment::Rest => decided = decided.rest(),
        }
    }
    std::borrow::Cow::Owned(decided)
}

/// What `person` may do in `workspace` because it is theirs alone: change their own records
/// ([`each_persons`]), such as their view of its layout, and read and change their private records
/// ([`has_member_reader`]). Decided for each of the workspace's people when they act
/// ([`crate::Party::owning_records`]), rather than kept as a grant for each of them, so a policy is
/// the size of its rules whatever its number of people; a ban or restriction, which no grant
/// lifts, stops it too.
#[must_use]
pub fn own_records(workspace: &str, person: &str) -> Vec<(TreePath, KindSet)> {
    let mut own: Vec<_> = persons_own(workspace, person)
        .map(|node| (node, KindSet::UNDOABLE))
        .collect();
    own.extend(app_fleet::every_private_kind().filter_map(|(app, kind)| {
        Some((
            TreePath::workspace(workspace)
                .app(app)
                .kind(kind)
                .resource(member_record(kind, person)?),
            KindSet::READ_UNDOABLE,
        ))
    }));
    own
}

/// The id of the grant that lets `app` keep its own records ([`app_records_grant`]).
#[must_use]
pub fn app_records_grant_id(app: &str) -> GrantId {
    GrantId(format!("app-records:{app}"))
}

/// The grant installing `app` in `workspace` gives it: reading and changing its own records, the
/// resources under its app id, when it acts by itself (a trigger, a migration, its own
/// connection). It is a person's grant to an app like any other, given by the member who
/// installed it (`installed_by`), so members see it among the workspace's grants and may revoke
/// it, and it stands only while that member reaches the app's records themselves
/// ([`Grant::stands_in`]). Anything beyond the app's own records still asks.
#[must_use]
pub fn app_records_grant(workspace: &str, app: &str, installed_by: &str) -> Grant {
    Grant {
        id: app_records_grant_id(app),
        grantee: Grantee::app(app),
        node: TreePath::workspace(workspace).app(app),
        kinds: KindSet::READ_UNDOABLE,
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by: Grantee::person(installed_by.to_owned()),
    }
}

/// Whether `grant` is the one installing its app gave it ([`app_records_grant`]): an app's own
/// records, on its own node.
#[must_use]
pub fn is_app_records_grant(grant: &Grant) -> bool {
    grant.grantee.app_id().is_some_and(|app| {
        grant.id == app_records_grant_id(app)
            && grant.node.id_at(Level::App) == Some(app)
            && grant.node.id_at(Level::Kind).is_none()
    })
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

/// The id of the grant that stops `app` carrying out one of its outside capabilities.
#[must_use]
pub fn outside_capability_grant_id(app: &str, capability: &str) -> GrantId {
    GrantId(format!("outside:{app}:{capability}"))
}

/// The app and capability a grant id names, when it names one of an app's outside
/// capabilities ([`outside_capability_grant_id`]).
#[must_use]
pub fn outside_capability_of(id: &str) -> Option<(&str, &str)> {
    let rest = id.strip_prefix("outside:")?;
    let (app, capability) = rest.split_once(':')?;
    (!app.is_empty() && !capability.is_empty()).then_some((app, capability))
}

/// The grant that stops `app` carrying out `capability` in `workspace`, given by `granted_by`: a
/// person turning one outside capability off on the app's page. It is a denial of outside effects
/// on the app's node, as any grant is, and its id names the one capability it stops
/// ([`outside_capability_of`]), so the app's other capabilities still go.
#[must_use]
pub fn outside_capability_denial(
    workspace: &str,
    app: &str,
    capability: &str,
    granted_by: Grantee,
) -> Grant {
    Grant {
        id: outside_capability_grant_id(app, capability),
        grantee: Grantee::app(app),
        node: TreePath::workspace(workspace).app(app),
        kinds: KindSet::OUTSIDE,
        answer: Answer::Deny,
        lifetime: Lifetime::UntilRevoked,
        granted_by,
    }
}

/// Whether `grant` stops `app` carrying out `capability`: a denial of outside effects on the
/// app's node that names no capability ([`restriction_grant`], a whole-app denial), or names this
/// one ([`outside_capability_denial`]).
#[must_use]
pub fn outside_denial_stops(grant: &Grant, app: &str, capability: &str) -> bool {
    grant.grantee == Grantee::app(app)
        && grant.answer == Answer::Deny
        && grant.kinds.contains(crate::ActionKind::Outside)
        && outside_capability_of(grant.id.as_str())
            .is_none_or(|(named_app, named)| named_app == app && named == capability)
}

/// The governance a Moderator may exercise: removing, banning and
/// restricting members, which the workspace keeps its owner through, and
/// naming the workspace, which is what people find it by.
pub const MODERATOR_GOVERNANCE: [&str; 6] = [
    governance::REMOVE_MEMBER,
    governance::BAN,
    governance::UNBAN,
    governance::RESTRICT,
    governance::UNRESTRICT,
    governance::RENAME,
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
                lifetime: *lifetime,
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
/// | Commenter | Read: Allow; comments, reactions, the discussion app: Allow (Undoable) |
/// | Editor | Read, Undoable: Allow; Outside: Ask; governance, moderation, people's views of the layout: Deny |
/// | Moderator | as Editor; moderation: Allow; `remove-member`, `ban`, `unban`, `restrict`, `unrestrict`, `rename`: Allow |
/// | Owner | everything: Allow |
#[must_use]
pub fn role_rules(role: Role, workspace: &str) -> Vec<Rule> {
    let mut rules = shown_role_rules(role, workspace);
    rules.extend(private_record_rules(workspace));
    if !matches!(role, Role::Owner) {
        rules.extend(
            app_fleet::WORKSPACE_DOCUMENTS
                .iter()
                .filter(|kept| kept.readers == app_fleet::Readers::Owner)
                .map(|kept| {
                    Rule::new(
                        TreePath::workspace(workspace).app(kept.app),
                        KindSet::ALL,
                        Answer::Deny,
                    )
                }),
        );
    }
    rules
}

fn shown_role_rules(role: Role, workspace: &str) -> Vec<Rule> {
    let workspace_node = TreePath::workspace(workspace);
    match role {
        Role::Viewer => vec![Rule::new(workspace_node, KindSet::READ, Answer::Allow)],
        Role::Commenter => vec![
            Rule::new(workspace_node, KindSet::READ, Answer::Allow),
            Rule::new(comments(workspace), KindSet::UNDOABLE, Answer::Allow),
            Rule::new(reactions(workspace), KindSet::UNDOABLE, Answer::Allow),
            Rule::new(discussion(workspace), KindSet::UNDOABLE, Answer::Allow),
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
    let mut rules = vec![
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
    ];
    rules.extend(
        each_persons(workspace).map(|(_, node)| Rule::new(node, KindSet::UNDOABLE, Answer::Deny)),
    );
    rules
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
            "Can do what an Editor can, moderate other people's content, and remove, ban or restrict members."
        }
        Role::Owner => "Can do everything, including the workspace's members and settings.",
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
        .collect()
}

/// Whether each of the role `wanted`'s grants gives no more than the role
/// `held` holds on its node ([`crate::WorkspacePolicy::check_within`]). A
/// role's grants are the same in every workspace, so each pair is decided
/// once, in a workspace of no one's.
pub(crate) fn role_within(wanted: Role, held: Role) -> bool {
    static WITHIN: std::sync::OnceLock<Vec<(Role, Role)>> = std::sync::OnceLock::new();
    let grants = |role: Role| {
        let grantee = Grantee::role(role);
        role_preset(role, "roles").assign(
            &role_grant_prefix(role),
            &grantee,
            &grantee,
            &Lifetime::UntilRevoked,
        )
    };
    WITHIN
        .get_or_init(|| {
            let pairs = Role::ALL
                .into_iter()
                .flat_map(|held| Role::ALL.map(|wanted| (wanted, held)));
            pairs
                .filter(|&(wanted, held)| {
                    let holding = grants(held);
                    let holder =
                        Authority::person(crate::Party::new(vec![Grantee::role(held)], &holding));
                    grants(wanted).iter().all(|grant| {
                        crate::WorkspacePolicy::check_within(&holder, grant, &Context::at(0))
                            .is_ok()
                    })
                })
                .collect()
        })
        .contains(&(wanted, held))
}

/// The id prefix of `role`'s built-in grants: `role:<role>`.
fn role_grant_prefix(role: Role) -> String {
    format!("role:{}", role.key())
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
/// An agent acts inside a turn ([`crate::turn::Turn`]), for the person whose
/// message caused it, bounded by its own person: it never holds more than their role,
/// and never more than [`ROLE_CAP`](agent_policy::ROLE_CAP). Every host — the
/// server hosting a workspace, every member admitting its ops, the desktop
/// broker hosting a workspace itself — asks [`role`](agent_policy::role) rather
/// than deciding on its own. A host admits only its own
/// person's agents.
///
/// Two questions about agents are still open. Until they are decided each
/// keeps the answer it was first given, and each is changed here:
///
/// 1. Is an agent's role capped at Editor? [`ROLE_CAP`](agent_policy::ROLE_CAP): yes.
/// 2. What does withdrawing an agent mean? [`WITHDRAWAL_IS_FINAL`](agent_policy::WITHDRAWAL_IS_FINAL): a
///    withdrawn agent device is never admitted again; granting it anew
///    needs a new agent key.
///
/// A member still admits an agent's ops by its delegation's workspaces and
/// expiry only: the app scope of its resource is enforced by the turn's action API.
pub mod agent_policy {
    use crate::Role;

    /// The most an agent ever acts as, whoever it acts for. An agent never
    /// acts as a workspace's owner.
    pub const ROLE_CAP: Role = Role::Editor;

    /// Whether withdrawing an agent from a workspace is final for its device
    /// (open question 2; see the module documentation).
    pub const WITHDRAWAL_IS_FINAL: bool = true;

    /// The role an agent acts with when its person holds `person`: theirs
    /// when [`ROLE_CAP`] holds all of its grants ([`Role::includes`]), and the
    /// cap otherwise.
    #[must_use]
    pub fn role(person: Role) -> Role {
        if ROLE_CAP.includes(person) {
            person
        } else {
            ROLE_CAP
        }
    }
}

/// The preset of [`agent_policy::ROLE_CAP`] the agent `agent` (its id and
/// its own person) acting for `person` in `workspace` carries, before any
/// bound: the delegation a person hands their agent
/// ([`crate::Acting::handed`]). Never more than the cap, never more than the
/// person may change, and never the workspace's governance. Only
/// [`crate::Acting::handed_preset`] makes it, so every API and every receiver
/// gives an agent the same.
#[must_use]
#[track_caller]
pub(crate) fn agent_preset(workspace: &str, agent: (&str, &str), person: &str) -> Vec<Grant> {
    let grantee = Grantee::agent(agent.0, agent.1);
    let agent = agent.0;
    let granted_by = Grantee::person(person.to_owned());
    let prefix = format!("turn:agent:{agent}");
    let mut grants = role_preset(agent_policy::ROLE_CAP, workspace).assign(
        &prefix,
        &grantee,
        &granted_by,
        &Lifetime::UntilRevoked,
    );
    // It changes its person's own records (their view of the layout) as they do, and no one
    // else's.
    grants.extend(persons_own(workspace, person).map(|node| Grant {
        id: GrantId(format!("{prefix}:view")),
        grantee: grantee.clone(),
        node,
        kinds: KindSet::UNDOABLE,
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by: granted_by.clone(),
    }));
    grants
}

/// The grants an agent's certificate gives it in `workspace`, `agent` its id and
/// its own person and `person` whom it acts for: the delegation
/// preset its person's certificate names for it ([`agent_preset`]), the one
/// place a certificate's rights become grants, so every host and every receiver
/// decides an agent alike. The workspace's own grants naming the agent narrow it
/// further.
#[must_use]
#[track_caller]
pub fn certified_agent(workspace: &str, agent: (&str, &str), person: &str) -> Vec<Grant> {
    agent_preset(workspace, agent, person)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writing a governance grant's id and reading it back name the same
    /// person: the reading follows the writing.
    #[test]
    fn the_governance_grant_ids_name_their_person() {
        for id in [
            administrator_grant_id("a person"),
            administrator_moderation_grant_id("a person"),
        ] {
            assert_eq!(administrator_named(&id), Some("a person"), "{id}");
        }
    }

    /// A capability's denial stops that capability alone, while a denial naming no capability
    /// stops the app's whole outside reach.
    #[test]
    fn an_outside_capabilitys_denial_names_the_one_it_stops() {
        let denial =
            outside_capability_denial("w", "so.tilo.mail", "smtp.send", Grantee::person("ada"));
        assert_eq!(
            outside_capability_of(denial.id.as_str()),
            Some(("so.tilo.mail", "smtp.send"))
        );
        assert!(outside_denial_stops(&denial, "so.tilo.mail", "smtp.send"));
        assert!(
            !outside_denial_stops(&denial, "so.tilo.mail", "imap.search"),
            "the app's other capabilities still go"
        );
        assert!(!outside_denial_stops(&denial, "so.tilo.tube", "smtp.send"));

        let whole = Grant::new(
            "no-outside",
            Grantee::app("so.tilo.mail"),
            TreePath::workspace("w").app("so.tilo.mail"),
            KindSet::OUTSIDE,
            Answer::Deny,
            Grantee::person("ada"),
        );
        for capability in ["smtp.send", "imap.search"] {
            assert!(
                outside_denial_stops(&whole, "so.tilo.mail", capability),
                "a denial naming no capability stops them all"
            );
        }
        let allow = Grant::new(
            "mail-may",
            Grantee::app("so.tilo.mail"),
            TreePath::workspace("w").app("so.tilo.mail"),
            KindSet::OUTSIDE,
            Answer::Allow,
            Grantee::person("ada"),
        );
        assert!(!outside_denial_stops(&allow, "so.tilo.mail", "smtp.send"));
        assert_eq!(outside_capability_of("no-outside"), None);
    }
}
