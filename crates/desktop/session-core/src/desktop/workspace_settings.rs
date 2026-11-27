//! What the workspace settings sheet reads and writes: the words for a workspace's roles, scopes
//! and grants; the draft a permission is composed from and the mutation it becomes; a member's role
//! and removal; admitting and summarising the agents and services that work in the workspace; who
//! owns it; its emoji and its default apps.
//! Members and permissions are both views of [`Workspace::policy`] — the single list
//! `decide_capability` consults, the sandbox turns into bwrap/Landlock rules, and the workspace
//! replicates. A member's role *is* the verb on their membership capability
//! ([`Workspace::role_of`]); nothing here keeps a parallel vocabulary that could drift from what is
//! enforced.
//! Every mutation routes through [`Workspace::grant_as`] / [`Workspace::revoke_as`] /
//! [`Workspace::govern_as_at`], which refuse callers failing [`Workspace::may_administer`].

use crate::desktop::i18n::{t, t_args};
use tilo_action_policy::{Grantee, Refusal};
use tilo_sort::CompactSort as _;

use collab_types::caps::{Access, CapOverride, DeviceCap, Effect, Scope, sandbox};
use collab_types::role::Role as CapRole;
use collab_types::{Member, MemberId};
use tilo_action_types::Lifetime;
use tilo_desktop_model::Workspace;

pub mod agent_access;
pub mod default_apps;

pub use default_apps::{
    DefaultAppChoice, default_app_choices, default_apps_status, next_user_default,
    next_workspace_default,
};
pub mod emoji_pane;
pub mod people;
mod people_pane;

pub use people_pane::member_row_offers;

/// The device's application catalogue, as workspace settings name apps, their actions and the
/// resources they open, and offer them as default openers.
pub trait AppDirectory {
    /// The name a person knows `app_id` by, when the host knows the app.
    fn app_display_name(&self, app_id: &str) -> Option<String>;
    /// The name the app's own catalogue publishes for its action `action` ("Post a message"), when
    /// the device knows it: the name the sheet asking a person to allow that action shows.
    fn action_display_name(&self, _app_id: &str, _action: &str) -> Option<String> {
        None
    }
    /// The apps installed for the workspace, by id: each one an agent may be given answers for
    /// before it ever touches the app, whether or not a window of it is open.
    fn installed_apps(&self) -> Vec<String> {
        Vec::new()
    }
    /// The name a person knows the resource at `address` by, as the resource itself publishes it (a
    /// page or an issue by its title), when the host has it.
    fn resource_label(&self, _address: &str) -> Option<String> {
        None
    }
}

/// A host with no application catalogue: an app is named by its id written as words, never shown as
/// an id.
pub struct NoAppDirectory;

impl AppDirectory for NoAppDirectory {
    fn app_display_name(&self, _app_id: &str) -> Option<String> {
        None
    }
}

fn app_display_name(apps: &dyn AppDirectory, app_id: &str) -> String {
    apps.app_display_name(app_id)
        .unwrap_or_else(|| tilo_desktop_search::names::app_name(app_id))
}

/// What one action of an app is called to the person: the name the app's own catalogue gives it, as
/// the sheet asking to allow it shows ("Post a message"), else its id in words.
fn action_name(apps: &dyn AppDirectory, app_id: &str, action: &str) -> String {
    apps.action_display_name(app_id, action)
        .unwrap_or_else(|| tilo_actions::action_label(action))
}

pub fn role_label(role: CapRole) -> String {
    t(match role {
        CapRole::Owner => "workspace-settings-role-owner",
        CapRole::Moderator => "workspace-settings-role-moderator",
        CapRole::Editor => "workspace-settings-role-editor",
        CapRole::Commenter => "workspace-settings-role-commenter",
        CapRole::Viewer => "workspace-settings-role-viewer",
    })
}

/// Plain-language consequences shown before changing a person's workspace role. Keeping this next
/// to the role vocabulary makes the chooser describe the same authority that `role_verb` ultimately
/// writes.
pub fn role_capability_preview(role: CapRole) -> String {
    t(match role {
        CapRole::Owner => "workspace-settings-role-owner-preview",
        CapRole::Moderator => "workspace-settings-role-moderator-preview",
        CapRole::Editor => "workspace-settings-role-editor-preview",
        CapRole::Commenter => "workspace-settings-role-commenter-preview",
        CapRole::Viewer => "workspace-settings-role-viewer-preview",
    })
}

/// What an administrator is called: an Editor who also administers the workspace.
#[must_use]
pub fn administrator_label() -> String {
    t("workspace-settings-role-administrator")
}

/// What an administrator may do, in the words the role chooser uses.
#[must_use]
pub fn administrator_capability_preview() -> String {
    t("workspace-settings-role-administrator-preview")
}

/// The catalogue message for a device-scoped capability, or `None` for a device the catalogue has
/// no name for (shown by its id).
fn device_scope_key(device: DeviceCap) -> Option<&'static str> {
    match device {
        DeviceCap::Camera => Some("workspace-settings-scope-camera"),
        DeviceCap::Microphone => Some("workspace-settings-scope-microphone"),
        DeviceCap::Screen => Some("workspace-settings-scope-screencast"),
        DeviceCap::Input => Some("workspace-settings-scope-remote-desktop"),
        _ => None,
    }
}

/// What a capability covers, on its own (a share row, a revoke toast).
pub fn scope_label(scope: &Scope, apps: &dyn AppDirectory) -> String {
    scope_phrase(scope, false, apps)
}

/// What a capability covers, phrased as the object of the sentence saying what it allows; languages
/// that inflect the object pick their own form through the `-object` messages.
fn scope_object_label(scope: &Scope, apps: &dyn AppDirectory) -> String {
    scope_phrase(scope, true, apps)
}

fn scope_phrase(scope: &Scope, object: bool, _apps: &dyn AppDirectory) -> String {
    let (key, args): (&str, Vec<(&str, tilo_ui_base::i18n::ArgValue)>) = match scope {
        Scope::FsPath(path) => fs_scope_phrase(path),
        Scope::FsResource(record) => (
            "workspace-settings-scope-record",
            vec![("id", short_id(record).into())],
        ),
        Scope::Device(device) => match device_scope_key(*device) {
            Some(key) => (key, Vec::new()),
            None => return device.name().to_owned(),
        },
        Scope::NetHost(host) if host == "*" => ("workspace-settings-scope-any-host", Vec::new()),
        Scope::NetHost(host) => ("workspace-settings-scope-host", vec![("host", host.into())]),
        Scope::NetCalls => ("workspace-settings-scope-calls", Vec::new()),
    };
    if object {
        t_args(&format!("{key}-object"), &args)
    } else {
        t_args(key, &args)
    }
}

/// A filesystem scope, phrased the way a person would say it. The [`WORKSPACE_FILES_PATH`] mount
/// point never appears in the label — it's a fact about where the OS keeps the bytes, not what the
/// user is agreeing to (HIG §4).
fn fs_scope_phrase(
    path: &str,
) -> (
    &'static str,
    Vec<(&'static str, tilo_ui_base::i18n::ArgValue)>,
) {
    // Trailing slash: `/var/workspace/` is the root, not an unnamed child.
    let trimmed = path.trim_end_matches('/');
    if trimmed == WORKSPACE_FILES_PATH {
        return ("workspace-settings-scope-workspace-files", Vec::new());
    }
    match trimmed.strip_prefix(&format!("{WORKSPACE_FILES_PATH}/")) {
        Some(rest) if !rest.is_empty() => (
            "workspace-settings-scope-workspace-folder",
            vec![("folder", rest.into())],
        ),
        _ => (
            "workspace-settings-scope-path",
            vec![("path", trimmed.into())],
        ),
    }
}

/// Shorten a long opaque identifier for display — person ids are
/// 64-char hex keys, too much to show in full on a row.
pub fn short_id(id: &str) -> String {
    if id.chars().count() <= 14 {
        return id.to_string();
    }
    let head: String = id.chars().take(10).collect();
    format!("{head}…")
}

/// One line describing what a capability allows, built per scope kind rather than concatenated —
/// "verb · scope" would read "View · the microphone" instead of "Use the microphone".
pub fn capability_allows(cap: &CapOverride, apps: &dyn AppDirectory) -> String {
    let key = match (&cap.scope, cap.access) {
        (Scope::Device(_), Access::Write) => "workspace-settings-allows-control",
        (Scope::Device(_), Access::Read) => "workspace-settings-allows-use",
        (Scope::NetHost(_), _) => "workspace-settings-allows-reach",
        (_, Access::Write) => "workspace-settings-allows-edit",
        (_, Access::Read) => "workspace-settings-allows-view",
    };
    t_args(
        key,
        &[("what", scope_object_label(&cap.scope, apps).into())],
    )
}

/// What one grant means, said as a person reads it: "Notes: reading and undoable changes allowed,
/// outside effects ask first". Where it says "everything", the grant covers every kind. An app's
/// answer to use another app says which: "Use “Post a message” in Chat".
pub fn grant_allows(grant: &tilo_action_policy::Grant, apps: &dyn AppDirectory) -> String {
    use tilo_action_policy::agent_answers::AnswerScope;
    // What installing an app gave it: its own records, listed under the app's name.
    if tilo_action_policy::roles::is_app_records_grant(grant) {
        return t("workspace-settings-allows-own-records");
    }
    let used = AnswerScope::of_node(&grant.node).filter(|_| {
        grant.grantee.app_id().is_some() && grant.answer == tilo_action_policy::Answer::Allow
    });
    match used {
        Some(AnswerScope::App { app }) => t_args(
            "workspace-settings-allows-use",
            &[("what", app_display_name(apps, &app).into())],
        ),
        Some(
            AnswerScope::Kind { app, kind: action }
            | AnswerScope::Action { app, action, .. }
            | AnswerScope::Resource { app, action, .. },
        ) => t_args(
            "workspace-settings-allows-action",
            &[
                ("action", action_name(apps, &app, &action).into()),
                ("app", app_display_name(apps, &app).into()),
            ],
        ),
        Some(AnswerScope::Workspace) | None => {
            grant_set_sentence(std::slice::from_ref(grant), apps)
        }
    }
}

/// The app names and resource titles a grant's path reads through, in the sheet's sentences.
struct Named<'a>(&'a dyn AppDirectory);

impl tilo_action_policy::Labels for Named<'_> {
    fn label(&self, level: tilo_action_policy::Level, id: &str) -> Option<String> {
        use tilo_action_policy::Level;
        match level {
            Level::App => Some(app_display_name(self.0, id)),
            Level::Resource | Level::Item => self.0.resource_label(id),
            _ => None,
        }
    }

    fn everything(&self) -> String {
        t("workspace-settings-scope-everything")
    }
}

/// A grant set as people and agents talk about it: one sentence per grant — where it applies, then
/// what it answers for each kind of action it covers — from the most specific node to the least. An
/// empty set grants nothing.
pub fn grant_set_sentence(grants: &[tilo_action_policy::Grant], apps: &dyn AppDirectory) -> String {
    tilo_action_policy::sentences(grants, &Named(apps))
}

/// Where a grant applies, named the way people know the workspace's parts: the app by its name, a
/// resource or item by its own, then the rest of the path.
pub fn node_phrase(node: &tilo_action_policy::TreePath, apps: &dyn AppDirectory) -> String {
    let below_workspace = node.within_workspace();
    if below_workspace.is_root() {
        return t("workspace-settings-scope-workspace");
    }
    tilo_action_policy::describe_node(&below_workspace, &Named(apps))
}

/// Who gave a grant or capability, as a user-facing phrase. People and agents are named as the
/// People pane names them; the viewer is "you".
fn given_by_label(who: &MemberId, ws: &Workspace, local_user: &str) -> String {
    if who.person_id() == Some(local_user) || who.agent_id() == Some(local_user) {
        return t("workspace-settings-granted-by-you");
    }
    let key = match who {
        MemberId::Person(_) => "workspace-settings-granted-by-person",
        MemberId::Agent { .. } => "workspace-settings-granted-by-agent",
        MemberId::Bot(_) => "workspace-settings-granted-by-bot",
    };
    t_args(key, &[("id", member_name_or_short_id(ws, who).into())])
}

/// Who gave a grant, as an access view draws them: the granter's name, what kind of author they
/// are (`you`, `person`, `agent`, `app`), and whether they are the viewer. The name and the kind
/// are drawn beside each other, never joined into one label.
#[must_use]
pub fn granted_by_parts(
    ws: &Workspace,
    who: &Grantee,
    local_user: &str,
    apps: &dyn AppDirectory,
) -> (String, &'static str, bool) {
    match who {
        Grantee::Member(id) => {
            let name = person_name(ws, local_user, id);
            let mine = id.person_id() == Some(local_user) || id.agent_id() == Some(local_user);
            let kind = if mine {
                "you"
            } else if id.person_id().is_some() {
                "person"
            } else {
                "agent"
            };
            (name, kind, mine)
        }
        Grantee::App(app) => (app_display_name(apps, app), "app", false),
        Grantee::Role(role) => (role_label(*role), "role", false),
        Grantee::CustomRole(id) => (
            ws.policy
                .custom_roles
                .get(id)
                .cloned()
                .unwrap_or_else(|| id.clone()),
            "role",
            false,
        ),
        Grantee::AnyKey => (who.to_string(), "visitor", false),
    }
}

/// Whether `person` may take `grant` back now: the one who gave an app grant, or whoever the
/// workspace lets revoke grants, unless they are barred from administering. What the host then
/// decides when the change is written.
#[must_use]
pub fn may_revoke(
    ws: &Workspace,
    person: &str,
    grant: &tilo_action_policy::Grant,
    now: u64,
) -> bool {
    if ws.policy.is_blocked(person)
        || ws
            .policy
            .restricted_until(person, &tilo_action_policy::Context::at_secs(now))
            .is_some()
    {
        return false;
    }
    let gave_it =
        matches!(grant.grantee, Grantee::App(_)) && grant.granted_by.person_id() == Some(person);
    gave_it
        || ws
            .policy
            .may_change_at(person, tilo_action_policy::roles::governance::REVOKE, now)
}

/// A subject the grant composer can target, and the subject axis the Permissions list groups by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum RecipientKind {
    App,
    Member,
    Agent,
    Role,
}

impl RecipientKind {
    pub fn section_label(self) -> String {
        t(match self {
            RecipientKind::App => "workspace-settings-section-apps",
            RecipientKind::Member => "workspace-settings-section-people",
            RecipientKind::Agent => "workspace-settings-section-agents",
            RecipientKind::Role => "workspace-settings-section-roles",
        })
    }
}

/// A resolved subject: what kind of thing it is and how it is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantRecipient {
    pub kind: RecipientKind,
    pub id: String,
}

impl GrantRecipient {
    /// Who a sandbox capability for this subject is held by, and the app it names: an app's
    /// sandbox belongs to the person granting it.
    pub fn to_parts(&self, person: &str) -> Option<(Grantee, Option<String>)> {
        match self.kind {
            RecipientKind::App => Some((Grantee::person(person), Some(self.id.clone()))),
            RecipientKind::Member => {
                Some((Grantee::Member(MemberId::parsed_person(&self.id)?), None))
            }
            RecipientKind::Agent => Some((
                Grantee::Member(MemberId::of_actor_bytes(self.id.as_bytes())?),
                None,
            )),
            // A role-wide grant is a policy decision, not a per-workspace click.
            RecipientKind::Role => None,
        }
    }

    pub fn display(&self, ws: &Workspace, local_user: &str, apps: &dyn AppDirectory) -> String {
        match self.kind {
            RecipientKind::Member => MemberId::parsed_person(&self.id).map_or_else(
                || self.id.clone(),
                |member| person_name(ws, local_user, &member),
            ),
            RecipientKind::Agent => MemberId::of_actor_bytes(self.id.as_bytes()).map_or_else(
                || self.id.clone(),
                |member| person_name(ws, local_user, &member),
            ),
            RecipientKind::App => app_display_name(apps, &self.id),
            // A role is listed as the phrase its section reads.
            RecipientKind::Role => ws
                .policy
                .custom_roles
                .get(&self.id)
                .cloned()
                .unwrap_or_else(|| self.id.clone()),
        }
    }
}

/// What the composer's scope picker maps to. Kept as a small enum rather than a pre-built `Scope`
/// so the table can stay `const` .
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantTarget {
    AnyHost,
    WorkspaceFiles,
}

/// One entry in the composer's "what does this allow" picker. Every entry produces a capability an
/// enforcement point actually reads (a network grant checks egress, `FsPath` checks the sandbox binds and
/// the filesystem). A device of a member's machine is never offered: only that person answers for
/// it, on their own device.
pub struct GrantChoice {
    /// What the choice is named by: the same key every host uses.
    pub key: &'static str,
    /// Catalogue key of the picker's label.
    label_key: &'static str,
    target: GrantTarget,
    access: Access,
}

impl GrantChoice {
    pub fn label(&self) -> String {
        t(self.label_key)
    }
}

static GRANT_CHOICES: &[GrantChoice] = &[
    GrantChoice {
        key: "network",
        label_key: "workspace-settings-grant-network",
        target: GrantTarget::AnyHost,
        access: Access::Read,
    },
    GrantChoice {
        key: "read-files",
        label_key: "workspace-settings-grant-read-files",
        target: GrantTarget::WorkspaceFiles,
        access: Access::Read,
    },
    GrantChoice {
        key: "edit-files",
        label_key: "workspace-settings-grant-edit-files",
        target: GrantTarget::WorkspaceFiles,
        access: Access::Write,
    },
];

/// Fallback for [`GrantDraft::choice`] if the index and [`GRANT_CHOICES`]'s length ever disagree —
/// unreachable in practice since the index is always taken modulo the live length, but a real value
/// beats a panic.
const FALLBACK_CHOICE: GrantChoice = GrantChoice {
    key: "edit-files",
    label_key: "workspace-settings-grant-edit-files",
    target: GrantTarget::WorkspaceFiles,
    access: Access::Write,
};

/// The path workspace files are projected at. A capability scoped here is enforced by the sandbox's
/// binds and by the filesystem alike.
const WORKSPACE_FILES_PATH: &str = "/var/workspace";

/// The in-flight grant the Permissions tab's composer is composing. Holds indices rather than
/// resolved values so a subject leaving the workspace can't leave a dangling reference —
/// [`grant_recipients`] is re-derived every frame and the index taken modulo its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantDraft {
    pub subject_index: usize,
    pub choice_index: usize,
    pub lifetime: Lifetime,
}

impl Default for GrantDraft {
    fn default() -> Self {
        Self {
            subject_index: 0,
            choice_index: 0,
            lifetime: Lifetime::UntilRevoked,
        }
    }
}

impl GrantDraft {
    pub fn cycle_choice(&mut self) {
        self.choice_index = self
            .choice_index
            .wrapping_add(1)
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
    }

    pub fn choice(&self) -> &'static GrantChoice {
        let idx = self
            .choice_index
            .checked_rem(GRANT_CHOICES.len())
            .unwrap_or(0);
        GRANT_CHOICES.get(idx).unwrap_or(&FALLBACK_CHOICE)
    }
}

/// What the composer's choices are named by where a view asks for the grant: whom it is for
/// (`member:…` , `agent:…` or `app:…` ) and the permission's key.
#[must_use]
pub fn grant_recipient_key(entry: &GrantRecipient) -> Option<String> {
    let kind = match entry.kind {
        RecipientKind::Member => "member",
        RecipientKind::Agent => "agent",
        RecipientKind::App => "app",
        RecipientKind::Role => return None,
    };
    Some(format!("{kind}:{}", entry.id))
}

/// The composer's permissions as (key, label), in picker order.
#[must_use]
pub fn grant_choices() -> Vec<(&'static str, String)> {
    GRANT_CHOICES
        .iter()
        .map(|choice| (choice.key, choice.label()))
        .collect()
}

/// The composer's lifetimes as (key, label), in picker order: what every prompt offers that the
/// workspace keeps, longest first. A one-use or session answer is the asking device's, never the
/// workspace's; the person picks the day and time an `until` one ends.
#[must_use]
pub fn lifetime_choices() -> Vec<(&'static str, String)> {
    Lifetime::offered(0)
        .into_iter()
        .rev()
        .filter(|lifetime| lifetime.is_durable())
        .map(|lifetime| {
            let key = lifetime.key();
            (key, tilo_ui_base::format::lifetime_kind(key))
        })
        .collect()
}

/// The draft a view asked for: the subject `subject` (`member:…`, `agent:…` or `app:…` ), the
/// permission `permission` , lasting until revoked or until `until` (Unix milliseconds), as the
/// composer stands at `now_ms`. `None` when the workspace holds no such subject, so a grant for
/// someone who left is never made for someone else, and for an end already past.
#[must_use]
pub fn draft_for(
    ws: &Workspace,
    subject: &str,
    permission: &str,
    until: Option<u64>,
    now_ms: u64,
) -> Option<GrantDraft> {
    let subject_index = grant_recipients(ws)
        .iter()
        .position(|entry| grant_recipient_key(entry).as_deref() == Some(subject))?;
    let choice_index = GRANT_CHOICES
        .iter()
        .position(|choice| choice.key == permission)?;
    let lifetime = until.map_or(Lifetime::UntilRevoked, Lifetime::Until);
    if !lifetime.is_live_at(Some(now_ms)) {
        return None;
    }
    Some(GrantDraft {
        subject_index,
        choice_index,
        lifetime,
    })
}

/// The subjects the composer can target, in picker order: the workspace's people first, then
/// admitted agents, then every app running in the workspace or already holding a capability —
/// without running apps, a host could only grant to an app already granted something.
pub fn grant_recipients(ws: &Workspace) -> Vec<GrantRecipient> {
    let mut out: Vec<GrantRecipient> = ws
        .policy
        .people()
        .into_iter()
        .map(|(user, _)| GrantRecipient {
            kind: RecipientKind::Member,
            id: user,
        })
        .collect();

    out.extend(
        ws.policy
            .agent_members_at(tilo_clock::unix_now())
            .into_iter()
            .map(|(agent, _)| GrantRecipient {
                kind: RecipientKind::Agent,
                id: String::from_utf8_lossy(&agent.actor_bytes()).into_owned(),
            }),
    );

    let mut apps: Vec<String> = Vec::new();
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                apps.extend(
                    tilo_desktop_model::apps::canonical_app_id(&tab.tile).map(String::from),
                );
            }
        }
    }
    for cap in &ws.sandbox {
        if let Some(app) = &cap.app {
            apps.push(app.as_str().to_owned());
        }
    }
    apps.compact_sort();
    apps.dedup();
    out.extend(apps.into_iter().map(|id| GrantRecipient {
        kind: RecipientKind::App,
        id,
    }));
    out
}

/// The capability `draft` names against `ws` , or `None` when the draft has no addressable subject
/// (an empty workspace) or names a subject this panel cannot grant to.
pub fn draft_capability(ws: &Workspace, person: &str, draft: &GrantDraft) -> Option<CapOverride> {
    let subjects = grant_recipients(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    let subject_entry = subjects.get(idx)?;
    let choice = draft.choice();
    let scope = match choice.target {
        GrantTarget::AnyHost => Scope::NetHost("*".to_string()),
        GrantTarget::WorkspaceFiles => Scope::FsPath(WORKSPACE_FILES_PATH.to_string()),
    };
    let (grantee, app) = subject_entry.to_parts(person)?;
    let cap = CapOverride::grant(grantee, scope, choice.access).lasting(draft.lifetime);
    Some(match app {
        Some(app) => cap.through(app),
        None => cap,
    })
}

/// What a panel mutation changed, for the caller that applies it to the desktop. Empty means there
/// was nothing to change.
pub use tilo_action_policy::PolicyEdit;

/// Set one person's or role's access to a published document.
pub fn set_document_access(
    ws: &mut Workspace,
    actor: &str,
    (who, app, document): (&str, &str, &str),
    access: &str,
) -> Result<PolicyEdit, Refusal> {
    let changes =
        collab_types::document_access_changes(&ws.policy, actor, (who, app, document), access)?;
    ws.policy
        .change_all_as_at(actor, changes, tilo_clock::unix_now())
}

/// Grant what `draft` names, as `actor`, through the workspace's governance:
/// the same check every action API and the workspace's host make.
///
/// # Errors
/// The workspace refused the grant, saying why.
pub fn apply_grant_draft(
    ws: &mut Workspace,
    actor: &str,
    draft: &GrantDraft,
) -> Result<PolicyEdit, Refusal> {
    let now = tilo_clock::unix_now();
    match draft_capability(ws, actor, draft) {
        Some(cap) => sandbox::grant_as_at(&ws.policy, &mut ws.sandbox, actor, cap, now),
        None => Ok(PolicyEdit::default()),
    }
}

/// Revoke the grant or sandbox capability with id `id`, as `actor`.
///
/// # Errors
/// The workspace refused the revocation, saying why.
pub fn apply_revoke(ws: &mut Workspace, actor: &str, id: &str) -> Result<PolicyEdit, Refusal> {
    let now = tilo_clock::unix_now();
    if ws.sandbox.iter().any(|cap| cap.id == id) {
        return sandbox::revoke_as_at(&ws.policy, &mut ws.sandbox, actor, id, now);
    }
    ws.policy.change_all_as_at(
        actor,
        vec![tilo_action_policy::Change::Revoke(
            tilo_action_policy::GrantId::new(id),
        )],
        now,
    )
}

/// Move `member` to `role`, as `actor`, through the workspace's governance. The
/// owner's role is structural and nobody changes their own.
///
/// # Errors
/// The workspace refused the change, saying why.
pub fn set_member_role(
    ws: &mut Workspace,
    actor: &str,
    member: &str,
    role: CapRole,
) -> Result<PolicyEdit, Refusal> {
    if role == CapRole::Owner {
        return Err(Refusal::OwnerRole);
    }
    if actor == member {
        return Err(Refusal::NotPermitted(crate::desktop::i18n::t(
            "workspace-settings-refused-own-role",
        )));
    }
    if ws.policy.is_owner(member) {
        return Err(Refusal::Owner);
    }
    let change = if ws.policy.members.contains_key(member) {
        tilo_action_policy::Change::SetRole {
            person: member.to_owned(),
            role,
        }
    } else {
        tilo_action_policy::Change::AddMember {
            person: member.to_owned(),
            role,
        }
    };
    ws.policy
        .change_all_as_at(actor, vec![change], tilo_clock::unix_now())
}

/// Remove `member` and every grant naming them, as `actor`.
///
/// # Errors
/// The workspace refused the removal, saying why.
pub fn remove_member(ws: &mut Workspace, actor: &str, member: &str) -> Result<PolicyEdit, Refusal> {
    // The owner is the workspace's root of trust; removing them would leave a workspace nobody
    // administers.
    if ws.policy.is_owner(member) {
        return Err(Refusal::Owner);
    }
    if actor == member {
        return Err(Refusal::NotPermitted(crate::desktop::i18n::t(
            "workspace-settings-refused-own-membership",
        )));
    }
    let now = tilo_clock::unix_now();
    let mut edit = ws.policy.change_all_as_at(
        actor,
        vec![tilo_action_policy::Change::RemoveMember {
            person: member.to_owned(),
        }],
        now,
    )?;
    // What the sandbox let them reach goes too.
    let held: Vec<String> = ws
        .sandbox
        .iter()
        .filter(|cap| grantee_is_person(&cap.grantee, member))
        .map(|cap| cap.id.clone())
        .collect();
    for id in held {
        if let Ok(revoked) = sandbox::revoke_as_at(&ws.policy, &mut ws.sandbox, actor, &id, now) {
            edit = edit.and(revoked);
        }
    }
    Ok(edit)
}

fn grantee_is_person(grantee: &Grantee, user: &str) -> bool {
    match grantee {
        Grantee::Member(MemberId::Person(id)) => id == user,
        Grantee::Member(MemberId::Agent { .. } | MemberId::Bot(_))
        | Grantee::Role(_)
        | Grantee::CustomRole(_)
        | Grantee::App(_)
        | Grantee::AnyKey => false,
    }
}

/// Revoke every grant naming `agent`, as `actor`.
pub fn remove_agent(ws: &mut Workspace, actor: &str, agent: &MemberId) -> PolicyEdit {
    let subject = tilo_action_policy::Grantee::Member(agent.clone());
    let revokes = ws
        .policy
        .grants
        .iter()
        .filter(|grant| grant.grantee == subject)
        .map(|grant| tilo_action_policy::Change::Revoke(grant.id.clone()))
        .collect();
    ws.policy
        .change_all_as_at(actor, revokes, tilo_clock::unix_now())
        .unwrap_or_default()
}

// Members pane

/// A member by name, or by a shortened id.
fn member_name_or_short_id(ws: &Workspace, id: &MemberId) -> String {
    member_named(ws, id).map_or_else(|| short_id(id.id()), |member| member.name.trim().to_owned())
}

/// The member `id` names, when the workspace's roster holds one: a person by their root key, an
/// agent by its key.
fn member_named<'a>(ws: &'a Workspace, id: &MemberId) -> Option<&'a Member> {
    ws.members
        .iter()
        .find(|member| member.id == *id && !member.name.trim().is_empty())
}

/// A member as the People pane names them: as the roster does, the viewer by their own name; only
/// someone the roster has never seen falls back to the workspace's record of them.
#[must_use]
pub fn member_row_name(
    names: &crate::desktop::people::PeopleNames,
    ws: &Workspace,
    local_user: &str,
    id: &MemberId,
) -> String {
    names
        .get(id)
        .map_or_else(|| person_name(ws, local_user, id), str::to_owned)
}

/// A person or agent as every workspace-settings pane names them: their member name, the
/// viewer included; the viewer by the name this device knows them by when the workspace has none;
/// else a shortened id.
fn person_name(ws: &Workspace, local_user: &str, id: &MemberId) -> String {
    let known = member_named(ws, id).is_some();
    if id.person_id() == Some(local_user)
        && !known
        && let Some(name) = ::collaboration::local_identity::load()
            .map(|identity| identity.display_name.trim().to_owned())
            .filter(|name| !name.is_empty())
    {
        return name;
    }
    member_name_or_short_id(ws, id)
}

/// What the People tab says about who may change a member's role, to a viewer who administers
/// none of them.
pub fn members_authority_note(administers: bool) -> Option<String> {
    (!administers).then(|| t("workspace-settings-owner-only"))
}

/// One entry the Permissions tab lists: a sandbox capability an app holds,
/// or a grant of the workspace's tree.
#[derive(Clone, Copy, Debug)]
enum Listed<'a> {
    Sandbox(&'a CapOverride),
    Grant(&'a tilo_action_policy::Grant),
}

impl Listed<'_> {
    fn id(&self) -> &str {
        match self {
            Self::Sandbox(cap) => &cap.id,
            Self::Grant(grant) => grant.id.as_str(),
        }
    }
}

/// The section a sandbox capability is listed in: the app whose sandbox it is, else its grantee's.
fn classify_capability(cap: &CapOverride) -> GrantRecipient {
    match &cap.app {
        Some(app) => classify_grantee(&Grantee::app(app.as_str())),
        None => classify_grantee(&cap.grantee),
    }
}

/// The section a grantee is listed in.
fn classify_grantee(grantee: &Grantee) -> GrantRecipient {
    match grantee {
        Grantee::Member(MemberId::Person(id)) => GrantRecipient {
            kind: RecipientKind::Member,
            id: id.as_str().to_owned(),
        },
        Grantee::Member(member @ (MemberId::Agent { .. } | MemberId::Bot(_))) => GrantRecipient {
            kind: RecipientKind::Agent,
            id: String::from_utf8_lossy(&member.actor_bytes()).into_owned(),
        },
        Grantee::App(app) => GrantRecipient {
            kind: RecipientKind::App,
            id: app.clone(),
        },
        Grantee::Role(role) => GrantRecipient {
            kind: RecipientKind::Role,
            id: t_args(
                "workspace-settings-subject-role",
                &[("role", role_label(*role).into())],
            ),
        },
        Grantee::CustomRole(id) => GrantRecipient {
            kind: RecipientKind::Role,
            id: id.clone(),
        },
        // Listed with the entry that sets it, never here (`listed_capabilities`).
        Grantee::AnyKey => GrantRecipient {
            kind: RecipientKind::Role,
            id: grantee.to_string(),
        },
    }
}

/// What the Permissions pane lists, grouped by subject kind, then subject,
/// then id: every sandbox capability granted, and every grant but the
/// role presets (the People tab is roles) and an agent's admission (the
/// Agents tab), or it would be revocable from neither tab. An end that has
/// passed gives nothing, so it is not listed either.
fn listed_capabilities(ws: &Workspace) -> Vec<(GrantRecipient, Listed<'_>)> {
    let live = Some(tilo_clock::unix_now().saturating_mul(1000));
    let mut out: Vec<(GrantRecipient, Listed<'_>)> = ws
        .sandbox
        .iter()
        .filter(|cap| cap.effect == Effect::Grant && cap.lifetime.is_live_at(live))
        .map(|cap| (classify_capability(cap), Listed::Sandbox(cap)))
        .collect();
    out.extend(
        ws.policy
            .grants
            .iter()
            .filter(|grant| grant.lifetime.is_live_at(live))
            .filter(|grant| {
                let preset =
                    grant.grantee.role_of().is_some() && grant.id.as_str().starts_with("role:");
                let admission = grant.grantee.agent_id().is_some()
                    && grant.id.as_str().starts_with("agent-member:");
                let document_access = grant.id.as_str().starts_with("document-access:");
                // An app's outside capability is the app's page's: it is turned off and on
                // there, and lists as a grant here never.
                let outside_capability =
                    tilo_action_policy::roles::outside_capability_of(grant.id.as_str()).is_some();
                // What visitors may do is the entry's setting, shown there.
                let entry = grant.grantee == Grantee::AnyKey;
                !preset && !admission && !document_access && !outside_capability && !entry
            })
            .map(|grant| (classify_grantee(&grant.grantee), Listed::Grant(grant))),
    );
    out.compact_sort_by(|a, b| {
        a.0.kind
            .cmp(&b.0.kind)
            .then_with(|| a.0.id.cmp(&b.0.id))
            .then_with(|| a.1.id().cmp(b.1.id()))
    });
    out
}

/// One permission the Permissions tab lists, in the viewer's words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListedPermission {
    /// What revoking it names.
    pub id: String,
    /// The group it is listed under: people, agents, apps and so on.
    pub section: String,
    /// Who holds it.
    pub holder: String,
    /// What it allows.
    pub allows: String,
    /// How long it lasts.
    pub expiry: String,
    /// Who gave it, where the workspace knows.
    pub granted_by: Option<String>,
}

/// The permissions the workspace gives, in the order the tab lists them,
/// each in the viewer's words.
#[must_use]
pub fn listed_permissions(
    ws: &Workspace,
    apps: &dyn AppDirectory,
    local_user: &str,
) -> Vec<ListedPermission> {
    listed_capabilities(ws)
        .into_iter()
        .map(|(subject, entry)| {
            let (allows, expiry, granted_by) = match entry {
                Listed::Sandbox(cap) => (
                    capability_allows(cap, apps),
                    tilo_ui_base::format::lifetime(cap.lifetime),
                    cap.granted_by
                        .as_ref()
                        .map(|who| given_by_label(who, ws, local_user)),
                ),
                Listed::Grant(grant) => (
                    grant_allows(grant, apps),
                    tilo_ui_base::format::lifetime(grant.lifetime),
                    match &grant.granted_by {
                        Grantee::Member(who) => Some(given_by_label(who, ws, local_user)),
                        Grantee::Role(_)
                        | Grantee::App(_)
                        | Grantee::CustomRole(_)
                        | Grantee::AnyKey => None,
                    },
                ),
            };
            ListedPermission {
                id: entry.id().to_owned(),
                section: subject.kind.section_label(),
                holder: subject.display(ws, local_user, apps),
                allows,
                expiry,
                granted_by,
            }
        })
        .collect()
}

#[cfg(test)]
mod members_tests {
    //! A member's role and removal rewrite the membership capability the model decides with, and
    //! only within owner authority.

    use super::*;
    use crate::desktop::authority::WorkspaceAuthority as _;
    use tilo_action_policy::{Answer, Change, Grant, roles};
    use tilo_action_types::{ActionKind, KindSet};
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const PEER: &str = "peer-user";

    fn ws_with_two_members() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from_u128(7), "members-test");
        ws.policy.set_owner(OWNER);
        assert!(
            set_member_role(&mut ws, OWNER, PEER, CapRole::Viewer)
                .is_ok_and(|edit| !edit.is_empty()),
            "owner may admit a member"
        );
        ws
    }

    /// Whether `who` may change the workspace's content now.
    fn may_write(ws: &Workspace, who: &str) -> bool {
        ws.policy.answer_at(
            who,
            &ws.policy.node(),
            ActionKind::Undoable,
            tilo_clock::unix_now(),
        ) == Answer::Allow
    }

    tilo_test_env::scenario! {
        fn members_are_named_and_removed_as_the_roster_holds_them;

        phase people_are_named_as_the_roster_names_them;
        {
            use crate::desktop::people::PeopleNames;
            let owner_key = "fa4834147f".repeat(6) + "0123";
            let mut ws = Workspace::new(WorkspaceId::from_u128(9), "Studio");
            ws.policy.set_owner(owner_key.as_str());
            assert!(
                set_member_role(&mut ws, &owner_key, "ada", CapRole::Editor)
                    .is_ok_and(|edit| !edit.is_empty())
            );
            // The roster knows Ada from presence; the workspace's own record of her has no name.
            let mut listed = ws.clone();
            listed.member_here(tilo_desktop_model::Member::named(collab_types::MemberId::person("ada"), "Ada"));
            let desktop = tilo_desktop_model::Layout {
                active: listed.id(),
                workspaces: vec![listed],
                ..tilo_desktop_model::Layout::default()
            };
            let names = PeopleNames::of(&desktop, &owner_key, Some("Robin"));
            assert_eq!(
                member_row_name(&names, &ws, &owner_key, &MemberId::person("ada")),
                "Ada",
                "the name the workspace lists, cased as it lists it"
            );
            let owner_name =
                member_row_name(&names, &ws, &owner_key, &MemberId::person(owner_key.clone()));
            assert_eq!(owner_name, "Robin", "the owner by their name");
            assert!(!owner_name.contains("fa48"), "never their key");

            assert_eq!(
                members_authority_note(true),
                None,
                "the owner is not told only the owner may"
            );
            assert_eq!(
                members_authority_note(false).as_deref(),
                Some("Only the workspace owner can change permissions.")
            );
        }

        phase member_removal_takes_back_what_the_member_held;
        {
            let mut ws = ws_with_two_members();
            let cap = CapOverride::grant(
                Grantee::person(PEER),
                Scope::FsPath("/var/workspace".into()),
                Access::Write,
            );
            ws.grant_as(OWNER, cap).expect("owner grants");
            let notes = Grant::new(
                "peer-notes",
                tilo_action_policy::Grantee::person(PEER),
                ws.policy.node().app("so.tilo.notes"),
                KindSet::READ_UNDOABLE,
                Answer::Allow,
                tilo_action_policy::Grantee::person(OWNER),
            );
            ws.policy.change_as_at(OWNER, Change::Grant(notes), tilo_clock::unix_now())
                .expect("owner shares an app");

            let edit = remove_member(&mut ws, OWNER, PEER)
                .expect("the owner may remove a member");
            assert_eq!(edit.members, vec![PEER.to_string()]);
            assert_eq!(edit.revoked.len(), 1, "the app grant");
            assert_eq!(edit.sandbox_revoked.len(), 1, "the file grant");
            assert_eq!(ws.role_of(PEER), None);
            assert!(
                !ws.allows(
                    &ws.acting_for_person(PEER),
                    &collab_types::caps::ResourceRef::FsPath("/var/workspace/notes".into()),
                    Access::Write
                ),
                "removing a member must revoke what they held, not just hide them"
            );

            let refusal = remove_member(&mut ws, OWNER, OWNER);
            assert!(refusal.is_err(), "the owner cannot be removed");
            assert_eq!(ws.role_of(OWNER), Some(CapRole::Owner));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn member_roles_come_from_the_roster_and_change_only_within_owner_authority() {
        {
            let mut ws = ws_with_two_members();
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            let edit = set_member_role(&mut ws, OWNER, PEER, CapRole::Editor)
                .expect("the owner may set a role");
            assert!(!edit.is_empty());
            assert_eq!(
                ws.role_of(PEER),
                Some(CapRole::Editor),
                "the role the sheet shows must be the role the model decides with"
            );
            assert!(may_write(&ws, PEER));
        }

        {
            let mut ws = ws_with_two_members();
            set_member_role(&mut ws, OWNER, PEER, CapRole::Editor)
                .expect("the owner may set a role");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Editor));
            set_member_role(&mut ws, OWNER, PEER, CapRole::Viewer)
                .expect("the owner may set a role");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            assert!(
                !may_write(&ws, PEER),
                "a demoted member must stop being able to write, not merely stop reading as Editor"
            );
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(&mut ws, PEER, PEER, CapRole::Owner);
            assert!(
                refusal.is_err(),
                "a viewer must not be able to promote itself"
            );
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(&mut ws, OWNER, OWNER, CapRole::Viewer);
            assert!(refusal.is_err(), "nobody changes their own role");
            assert_eq!(ws.role_of(OWNER), Some(CapRole::Owner));
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(&mut ws, OWNER, PEER, CapRole::Owner);
            assert!(refusal.is_err(), "ownership is not handed out");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            assert_eq!(ws.policy.owner(), Some(OWNER));
        }

        {
            let mut ws = ws_with_two_members();
            let admin = Grant::new(
                "peer-administers",
                tilo_action_policy::Grantee::person(PEER),
                roles::workspace_resource(&ws.policy.workspace),
                KindSet::ALL,
                Answer::Allow,
                tilo_action_policy::Grantee::person(OWNER),
            );
            ws.policy
                .change_as_at(OWNER, Change::Grant(admin), tilo_clock::unix_now())
                .expect("owner delegates administration");
            let before = ws.policy.clone();

            assert!(set_member_role(&mut ws, PEER, PEER, CapRole::Viewer,).is_err());
            assert!(remove_member(&mut ws, PEER, PEER).is_err());
            assert_eq!(ws.policy, before);
            assert!(ws.may_administer(PEER));
        }
    }
}

#[cfg(test)]
mod agents_tests {
    //! An agent is a permission subject like any other: what it may do reads back from the
    //! workspace's grants naming it, per subject.

    use super::*;
    use crate::desktop::authority::WorkspaceAuthority as _;
    use tilo_action_policy::{Answer, Change, Grant};
    use tilo_action_types::{ActionKind, KindSet};
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe-agent";

    fn agent() -> MemberId {
        MemberId::agent(AGENT, OWNER)
    }

    fn ws_with_agent() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from_u128(17), "agents-test");
        ws.policy.set_owner(OWNER);
        ws.policy.grant(Grant::new(
            format!("agent-member:{AGENT}"),
            tilo_action_policy::Grantee::agent(AGENT, OWNER),
            ws.policy.node(),
            KindSet::READ,
            Answer::Allow,
            tilo_action_policy::Grantee::person(OWNER),
        ));
        let notes = Grant::new(
            "scribe-notes",
            tilo_action_policy::Grantee::agent(AGENT, OWNER),
            ws.policy.node().app("so.tilo.notes"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            tilo_action_policy::Grantee::person(OWNER),
        );
        ws.policy
            .change_as_at(OWNER, Change::Grant(notes), tilo_clock::unix_now())
            .expect("owner grants scoped access");
        ws
    }

    fn agent_answer(ws: &Workspace, app: &str, kind: ActionKind) -> Answer {
        ws.policy
            .party_of_agent(AGENT, OWNER)
            .decide(
                &ws.policy.node().app(app),
                kind,
                &tilo_action_policy::Context::at_secs(tilo_clock::unix_now()),
            )
            .answer
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn agents_are_scoped_permission_subjects_presented_honestly() {
        {
            let ws = ws_with_agent();
            let members = ws.policy.agent_members_at(tilo_clock::unix_now());
            assert_eq!(members.len(), 1, "listed with the workspace's agents");
            assert_eq!(members[0].0, agent());
            assert_eq!(
                members[0].1,
                KindSet::READ,
                "the workspace grant admits it; app grants are its own"
            );
        }

        {
            let mut ws = Workspace::new(WorkspaceId::from_u128(18), "profile-test");
            ws.policy.set_owner(OWNER);
            ws.policy.grant(Grant::new(
                format!("agent-member:{AGENT}"),
                tilo_action_policy::Grantee::agent(AGENT, OWNER),
                ws.policy.node(),
                KindSet::READ,
                Answer::Allow,
                tilo_action_policy::Grantee::person(OWNER),
            ));
            assert_eq!(
                ws.policy.agent_members_at(tilo_clock::unix_now()),
                vec![(agent(), KindSet::READ)]
            );
            assert_eq!(
                agent_answer(&ws, "so.tilo.notes", ActionKind::Read),
                Answer::Allow
            );
            assert_eq!(
                agent_answer(&ws, "so.tilo.notes", ActionKind::Undoable),
                Answer::Deny,
                "reading the workspace is no license to change it"
            );

            ws.grant_as(
                OWNER,
                CapOverride::grant(
                    Grantee::agent(AGENT, OWNER),
                    Scope::FsPath("/var/workspace/brief.md".into()),
                    Access::Read,
                ),
            )
            .expect("owner grants one selected file");
        }

        {
            let ws = ws_with_agent();
            assert_eq!(
                agent_answer(&ws, "so.tilo.notes", ActionKind::Undoable),
                Answer::Allow
            );
            assert_eq!(
                agent_answer(&ws, "so.tilo.sheets", ActionKind::Undoable),
                Answer::Deny,
                "a grant must not become ambient human-role authority",
            );
        }

        {
            let mut ws = ws_with_agent();
            let edit = remove_agent(&mut ws, OWNER, &agent());
            assert_eq!(
                edit.revoked.len(),
                2,
                "its workspace grant and its app grant"
            );
            assert!(
                ws.policy
                    .agent_members_at(tilo_clock::unix_now())
                    .is_empty()
            );
            assert_eq!(
                agent_answer(&ws, "so.tilo.notes", ActionKind::Undoable),
                Answer::Deny
            );
        }

        {
            let mut ws = ws_with_agent();
            let before = ws.policy.clone();
            assert!(remove_agent(&mut ws, "viewer", &agent()).is_empty());
            assert_eq!(ws.policy, before);
        }

        {
            let ws = ws_with_agent();
            let subjects = grant_recipients(&ws);
            let index = subjects
                .iter()
                .position(|subject| {
                    subject.kind == RecipientKind::Agent
                        && MemberId::of_actor_bytes(subject.id.as_bytes()) == Some(agent())
                })
                .expect("agent is offered in the permission composer");
            let draft = GrantDraft {
                subject_index: index,
                ..GrantDraft::default()
            };
            let Some(cap) = draft_capability(&ws, OWNER, &draft) else {
                panic!("agent grant can be composed");
            };
            assert_eq!(cap.grantee, Grantee::agent(AGENT, OWNER));
        }

        {
            let ws = ws_with_agent();
            let listed = listed_permissions(&ws, &NoAppDirectory, OWNER);
            assert_eq!(
                listed
                    .iter()
                    .filter(|row| row.section == RecipientKind::Agent.section_label())
                    .count(),
                1,
                "only the explicit app grant belongs in Privacy & Security",
            );
            assert!(listed.iter().all(|row| !row.id.is_empty()));
        }
    }
}

#[cfg(test)]
mod permissions_tests {
    //! The Permissions tab lists the workspace's grants in the viewer's words, and its composer
    //! writes one through the owner's authority.

    use super::*;
    use crate::desktop::authority::WorkspaceAuthority as _;
    use collab_types::caps::ResourceRef;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner-user";
    /// Whoever is looking at the tab; never a grantor in these fixtures.
    const VIEWER: &str = "viewer-user";

    fn app_acting(app: &str) -> tilo_member_types::Acting {
        tilo_member_types::Acting::person(OWNER).in_app(app)
    }

    fn ws_with_grants() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::from_u128(3), "perm-test");
        ws.policy.set_owner(OWNER);
        sandbox::grant(
            &mut ws.sandbox,
            CapOverride::grant(
                Grantee::person(OWNER),
                Scope::Device(DeviceCap::Camera),
                Access::Read,
            )
            .through("firefox"),
        );
        sandbox::grant(
            &mut ws.sandbox,
            CapOverride::grant(
                Grantee::person(OWNER),
                Scope::NetHost("*".into()),
                Access::Write,
            )
            .through("firefox")
            .lasting(Lifetime::Until(u64::MAX)),
        );
        ws
    }

    fn listed(ws: &Workspace) -> Vec<ListedPermission> {
        listed_permissions(ws, &NoAppDirectory, VIEWER)
    }

    /// Tomorrow at `hour`:`minute`, in Unix milliseconds: the end the composer's date field picks.
    fn tomorrow_at(hour: i8, minute: i8) -> u64 {
        let day = tilo_ui_base::clock::today().tomorrow().expect("a tomorrow");
        let time = tilo_ui_base::format::Time::new(hour, minute, 0, 0).expect("a time of day");
        let at =
            tilo_ui_base::clock::instant_of(day.to_datetime(time)).expect("the end exists locally");
        u64::try_from(at.as_millisecond()).expect("after the epoch")
    }

    #[test]
    fn a_lifetime_that_ends_says_when_it_ends() {
        assert_eq!(
            tilo_ui_base::format::lifetime(Lifetime::UntilRevoked),
            "Always"
        );
        assert_eq!(
            tilo_ui_base::format::lifetime(Lifetime::Until(tomorrow_at(17, 30))),
            "Until Tomorrow 5:30 PM",
            "a timed grant shows its end in the composer's own date words"
        );
        let labels: Vec<String> = lifetime_choices()
            .into_iter()
            .map(|(_, label)| label)
            .collect();
        assert_eq!(
            labels,
            ["Always", "Until"],
            "the composer's choice names the kind, and only a grant names its end"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn permission_rows_list_each_grant_with_its_real_scope_and_subject() {
        {
            let record = "b0d4e23f-c7cb-4ded-a427-b32cf9da9341";
            assert_eq!(
                scope_label(&Scope::FsResource(record.to_string()), &NoAppDirectory),
                "one shared file (b0d4e23f-c…)"
            );
            assert_eq!(
                capability_allows(
                    &CapOverride::grant(
                        Grantee::person(OWNER),
                        Scope::FsResource(record.to_string()),
                        Access::Read,
                    )
                    .through("files"),
                    &NoAppDirectory
                ),
                "View one shared file (b0d4e23f-c…)"
            );
        }

        {
            let ws = ws_with_grants();
            let rows = listed(&ws);
            assert_eq!(rows.len(), 2);
            for row in &rows {
                assert_eq!(row.section, RecipientKind::App.section_label());
                assert_eq!(
                    row.holder, "Firefox",
                    "an app is named, never shown as its id"
                );
                assert!(!row.id.is_empty(), "rows are keyed by capability id");
            }
            assert_ne!(rows[0].id, rows[1].id);
        }

        {
            let mut ws = ws_with_grants();
            ws.grant_as(
                OWNER,
                CapOverride::grant(
                    Grantee::person("peer-user"),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                ),
            )
            .expect("owner grants");
            ws.policy
                .change_as_at(
                    OWNER,
                    tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                        "editors-camera-app",
                        tilo_action_policy::Grantee::role(CapRole::Editor),
                        ws.policy.node().app("so.tilo.camera"),
                        tilo_action_types::KindSet::READ,
                        tilo_action_policy::Answer::Allow,
                        tilo_action_policy::Grantee::person(OWNER),
                    )),
                    tilo_clock::unix_now(),
                )
                .expect("owner grants");
            let rows = listed(&ws);
            let kinds: Vec<&str> = rows.iter().map(|r| r.section.as_str()).collect();
            for expected in [
                RecipientKind::App.section_label(),
                RecipientKind::Member.section_label(),
                RecipientKind::Role.section_label(),
            ] {
                assert!(kinds.contains(&expected.as_str()), "{kinds:?}");
            }
            let mut sorted = kinds.clone();
            sorted.sort_unstable();
            assert_eq!(kinds, sorted, "rows are grouped by subject kind");
        }

        {
            // A 64-hex person key is what the model records; the row names the member, and
            // the viewer's own grant reads "you".
            let ada = "ad".repeat(32);
            let bea = "be".repeat(32);
            let mut ws = Workspace::new(WorkspaceId::from_u128(5), "names");
            ws.policy.set_owner(&ada);
            ws.members_here([
                tilo_desktop_model::Member::named(
                    collab_types::MemberId::person(ada.clone()),
                    "Ada Lovelace",
                ),
                tilo_desktop_model::Member::named(
                    collab_types::MemberId::person(bea.clone()),
                    "Bea",
                ),
            ]);
            ws.grant_as(
                &ada,
                CapOverride::grant(
                    Grantee::person(bea.clone()),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                ),
            )
            .expect("owner grants");
            let as_bea = listed_permissions(&ws, &NoAppDirectory, VIEWER);
            assert_eq!(as_bea[0].holder, "Bea");
            assert_eq!(
                as_bea[0].granted_by.as_deref(),
                Some("granted by Ada Lovelace")
            );
            let as_ada = listed_permissions(&ws, &NoAppDirectory, &ada);
            assert_eq!(as_ada[0].granted_by.as_deref(), Some("granted by you"));
            let as_bea = listed_permissions(&ws, &NoAppDirectory, &bea);
            assert_eq!(
                as_bea[0].holder, "Bea",
                "the viewer is named, never \"You\""
            );
            for row in as_bea.iter().chain(&as_ada) {
                let shown = format!("{} {}", row.holder, row.granted_by.as_deref().unwrap_or(""));
                assert!(
                    !shown.contains("adad") && !shown.contains("bebe"),
                    "{shown}"
                );
            }
        }

        {
            let mut ws = Workspace::new(WorkspaceId::from_u128(4), "labels");
            ws.policy.set_owner(OWNER);
            ws.grant_as(
                OWNER,
                CapOverride::grant(
                    Grantee::person(OWNER),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                )
                .through("terminal")
                .lasting(Lifetime::Until(tomorrow_at(17, 30))),
            )
            .expect("owner grants");
            let row = &listed(&ws)[0];
            assert_eq!(row.allows, "Edit workspace files");
            assert_eq!(row.expiry, "Until Tomorrow 5:30 PM");
            assert_eq!(
                row.granted_by.as_deref(),
                Some("granted by owner-user"),
                "grant_as records the grantor; the row shows it"
            );
        }

        // No capability line may name the mount point workspace files are projected at (HIG §4).
        // Swept over every scope kind, not just the two the panel shows today, so a new scope can't
        // reintroduce the leak.
        {
            let scopes = vec![
                Scope::FsPath(WORKSPACE_FILES_PATH.into()),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/")),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design/drafts")),
                Scope::Device(DeviceCap::Camera),
                Scope::NetHost("*".into()),
                Scope::NetHost("example.com".into()),
                Scope::FsResource("b0d4e23f-c7cb-4ded-a427-b32cf9da9341".into()),
            ];
            for scope in &scopes {
                for verb in [Access::Read, Access::Write] {
                    let cap = CapOverride::grant(Grantee::person(OWNER), scope.clone(), verb)
                        .through("a");
                    let line = capability_allows(&cap, &NoAppDirectory);
                    assert!(
                        !line.contains(WORKSPACE_FILES_PATH),
                        "the permission line {line:?} names {WORKSPACE_FILES_PATH}"
                    );
                    assert!(
                        !line.contains("/var/"),
                        "the permission line {line:?} exposes a system path"
                    );
                    assert!(!line.is_empty(), "{scope:?} produced an empty line");
                }
            }
        }

        // The projection root and a path inside it must read as different things, so a row never
        // understates what is being granted.
        {
            let root = CapOverride::grant(
                Grantee::person(OWNER),
                Scope::FsPath(WORKSPACE_FILES_PATH.into()),
                Access::Write,
            )
            .through("a");
            let sub = CapOverride::grant(
                Grantee::person(OWNER),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
                Access::Write,
            )
            .through("a");
            assert_eq!(
                capability_allows(&root, &NoAppDirectory),
                "Edit workspace files"
            );
            assert_eq!(
                capability_allows(&sub, &NoAppDirectory),
                "Edit \u{201c}design\u{201d} in this workspace",
            );
            assert_ne!(
                capability_allows(&root, &NoAppDirectory),
                capability_allows(&sub, &NoAppDirectory),
                "granting every workspace file must not read the same as granting \
                 one folder in it",
            );
        }
    }

    #[test]
    fn the_composer_offers_what_the_workspace_keeps() {
        let keys: Vec<_> = lifetime_choices().into_iter().map(|(key, _)| key).collect();
        assert_eq!(
            keys,
            ["always", "until"],
            "only what the workspace keeps: never once or this session"
        );
        assert_eq!(keys[0], "always", "the composer starts on a lasting grant");
        let ws = ws_with_grants();
        // One Firefox does not hold yet, so the grant is not merged into a longer one.
        let permission = "read-files";
        let draft = draft_for(&ws, "app:firefox", permission, None, 10).expect("a draft");
        assert_eq!(draft.lifetime, Lifetime::UntilRevoked);
        let draft = draft_for(&ws, "app:firefox", permission, Some(20), 10).expect("a draft");
        assert_eq!(draft.lifetime, Lifetime::Until(20));
        assert!(
            draft_for(&ws, "app:firefox", permission, Some(10), 10).is_none(),
            "an end already past composes nothing"
        );
        let mut granted = ws;
        apply_grant_draft(&mut granted, OWNER, &draft).expect("the owner grants");
        assert!(
            granted
                .sandbox
                .iter()
                .any(|cap| cap.lifetime == Lifetime::Until(20))
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn granting_and_revoking_permissions_is_owner_only_and_enforced() {
        {
            let ws = ws_with_grants();
            let subjects = grant_recipients(&ws);
            assert_eq!(
                subjects[0],
                GrantRecipient {
                    kind: RecipientKind::Member,
                    id: OWNER.to_string()
                },
                "people come first"
            );
            assert!(subjects.contains(&GrantRecipient {
                kind: RecipientKind::App,
                id: "firefox".to_string()
            }));
        }

        {
            let mut ws = ws_with_grants();
            let subjects = grant_recipients(&ws);
            let firefox = subjects
                .iter()
                .position(|s| s.id == "firefox")
                .expect("firefox is a candidate subject");
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                lifetime: Lifetime::UntilRevoked,
            };
            while draft.choice().label() != "Read workspace files" {
                draft.cycle_choice();
            }
            assert!(!ws.allows(
                &app_acting("firefox"),
                &ResourceRef::FsPath(WORKSPACE_FILES_PATH.to_string()),
                Access::Read
            ));

            let edit = apply_grant_draft(&mut ws, OWNER, &draft).expect("the owner may grant");
            assert_eq!(edit.sandbox_granted.len(), 1);
            assert!(
                ws.allows(
                    &app_acting("firefox"),
                    &ResourceRef::FsPath(WORKSPACE_FILES_PATH.to_string()),
                    Access::Read
                ),
                "the granted capability must start working, not merely start rendering"
            );
        }

        {
            let mut ws = ws_with_grants();
            let firefox = grant_recipients(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            // Re-granting an existing capability merges expiries, so pick something the app doesn't
            // already hold to observe this one.
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                lifetime: Lifetime::Until(5_000),
            };
            while draft.choice().label() != "Read workspace files" {
                draft.cycle_choice();
            }
            let edit = apply_grant_draft(&mut ws, OWNER, &draft).expect("the owner may grant");
            let id = &edit.sandbox_granted[0];
            let cap = ws
                .sandbox
                .iter()
                .find(|c| &c.id == id)
                .expect("stored capability");
            assert_eq!(cap.lifetime, Lifetime::Until(5_000));
        }

        {
            // `grant_capability` keeps the longer expiry for the same (subject, scope, effect,
            // verb).
            let mut ws = ws_with_grants();
            let firefox = grant_recipients(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                lifetime: Lifetime::Until(5_000),
            };
            while draft.choice().label() != "Read workspace files" {
                draft.cycle_choice();
            }
            let always = GrantDraft {
                lifetime: Lifetime::UntilRevoked,
                ..draft
            };
            apply_grant_draft(&mut ws, OWNER, &always).expect("the owner may grant");
            apply_grant_draft(&mut ws, OWNER, &draft).expect("the owner may grant");
            let files = ws
                .sandbox
                .iter()
                .filter(|c| matches!(&c.scope, Scope::FsPath(_)))
                .collect::<Vec<_>>();
            assert_eq!(
                files.len(),
                1,
                "the re-grant merges rather than duplicating"
            );
            assert_eq!(
                files[0].lifetime,
                Lifetime::UntilRevoked,
                "the existing permanent grant is not demoted to a timed one"
            );
            let row = listed(&ws)
                .into_iter()
                .find(|r| r.id == files[0].id)
                .expect("files row");
            assert_eq!(
                row.expiry, "Always",
                "the row must report the expiry that is stored, not the one that was asked for"
            );
        }

        {
            let mut ws = ws_with_grants();
            let firefox = grant_recipients(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            let draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                lifetime: Lifetime::UntilRevoked,
            };
            let before = ws.sandbox.len();
            assert!(
                apply_grant_draft(&mut ws, "someone-else", &draft).is_err(),
                "grant_as must refuse a non-administrator"
            );
            assert_eq!(ws.sandbox.len(), before);
        }

        {
            let ws = ws_with_grants();
            let firefox = grant_recipients(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            for (i, choice) in GRANT_CHOICES.iter().enumerate() {
                let draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: i,
                    lifetime: Lifetime::UntilRevoked,
                };
                let mut probe = ws.clone();
                let cap = draft_capability(&ws, OWNER, &draft).expect("every choice resolves");
                assert_eq!(cap.effect, Effect::Grant);
                let resource = match &cap.scope {
                    Scope::Device(d) => ResourceRef::Device(*d),
                    Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
                    Scope::NetCalls => ResourceRef::NetCalls,
                    Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
                    Scope::FsResource(record) => {
                        panic!("unexpected scope in the grant table: {record:?}")
                    }
                };
                probe.grant_as(OWNER, cap.clone()).expect("owner grants");
                assert!(
                    probe.allows(&app_acting("firefox"), &resource, cap.access),
                    "granting {:?} must make the decision function allow it",
                    choice.label()
                );
            }
        }

        {
            let mut ws = ws_with_grants();
            let camera_row = listed(&ws)
                .into_iter()
                .find(|r| r.allows.contains("camera"))
                .expect("camera row");
            let edit = apply_revoke(&mut ws, OWNER, &camera_row.id).expect("the owner may revoke");
            assert_eq!(edit.sandbox_revoked, vec![camera_row.id.clone()]);
            assert!(
                !ws.allows(
                    &app_acting("firefox"),
                    &ResourceRef::Device(DeviceCap::Camera),
                    Access::Read
                ),
                "the revoked capability must stop working, not merely stop rendering"
            );
            assert_eq!(listed(&ws).len(), 1);
        }

        {
            let mut ws = ws_with_grants();
            let target = listed(&ws)[0].id.clone();
            let refusal = apply_revoke(&mut ws, "someone-else", &target);
            assert!(refusal.is_err(), "only an administrator may revoke");
            assert_eq!(listed(&ws).len(), 2);
        }

        {
            let mut ws = ws_with_grants();
            let refusal = apply_revoke(&mut ws, OWNER, "cap-does-not-exist");
            assert!(refusal.is_err(), "there is no such grant to revoke");
            assert_eq!(ws.sandbox.len(), 2, "the two grants");
        }
    }

    tilo_test_env::scenario! {
        fn a_grant_names_its_resource_and_reads_back_as_the_workspace_holds_it;

        phase a_grant_set_reads_back_as_the_grants_the_workspace_holds;
        {
            let mut ws = ws_with_grants();
            let draft = draft_for(&ws, "app:firefox", "read-files", None, 0).expect("a draft");
            let edit = apply_grant_draft(&mut ws, OWNER, &draft).expect("owner grants");
            assert_eq!(edit.sandbox_granted.len(), 1);
            let rows = listed(&ws);
            assert_eq!(
                rows.len(),
                ws.sandbox
                    .iter()
                    .filter(|c| c.effect == Effect::Grant)
                    .count(),
                "every live grant is listed once"
            );
            for row in &rows {
                assert!(ws.sandbox.iter().any(|cap| cap.id == row.id));
            }
        }
    }

    /// Chat's catalogue as a device knows it: its conversation resource names "post".
    struct ChatCatalogue;

    impl AppDirectory for ChatCatalogue {
        fn app_display_name(&self, _app_id: &str) -> Option<String> {
            None
        }

        fn action_display_name(&self, app_id: &str, action: &str) -> Option<String> {
            (app_id == "so.tilo.chat" && action == "post").then(|| "Post a message".to_owned())
        }
    }

    /// An app's grant to use another app's action reads as the action's own catalogue names it,
    /// the name the sheet asking for it shows; an action the catalogue does not name keeps its
    /// id in words.
    #[test]
    fn an_action_grant_reads_as_the_apps_catalogue_names_the_action() {
        use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
        use tilo_action_policy::{Answer, Grantee};
        use tilo_action_types::ActionKind;

        let mut ws = Workspace::new(WorkspaceId::from_u128(6), "action-names");
        ws.policy.set_owner(OWNER);
        let front = ws.id().to_string();
        let grant = |action: &str| {
            answer_grant(
                &front,
                Grantee::app("so.tilo.notes"),
                (
                    &AnswerScope::action("so.tilo.chat", "chat.conversation", action),
                    ActionKind::Undoable,
                ),
                Answer::Allow,
                Grantee::person(OWNER),
            )
        };
        ws.policy.grant(grant("post"));
        let named = listed_permissions(&ws, &ChatCatalogue, VIEWER);
        assert_eq!(
            named[0].allows,
            "Use \u{201c}Post a message\u{201d} in Chat"
        );

        ws.policy.grant(grant("post-view"));
        let unnamed = listed_permissions(&ws, &ChatCatalogue, VIEWER)
            .into_iter()
            .find(|row| row.allows.contains("Post view"))
            .expect("the catalogue names no post-view");
        assert_eq!(unnamed.allows, "Use \u{201c}Post view\u{201d} in Chat");
    }

    /// What installing an app gave it lists under the app's name, as keeping its own records,
    /// given by the member who installed it; whoever may revoke a grant there revokes it.
    #[test]
    fn an_apps_own_records_list_under_its_name_and_revoke() {
        let mut ws = Workspace::new(WorkspaceId::from_u128(7), "own-records");
        ws.policy.set_owner(OWNER);
        let front = ws.id().to_string();
        let grant = tilo_action_policy::roles::app_records_grant(&front, "so.tilo.clocks", OWNER);
        ws.policy.grant(grant.clone());
        let listed = listed_permissions(&ws, &ChatCatalogue, OWNER);
        let row = listed
            .iter()
            .find(|row| row.id == grant.id.as_str())
            .expect("the grant installing Clocks gave is listed");
        assert_eq!(row.holder, "Clocks");
        assert_eq!(row.allows, "Keep its own records");
        assert_eq!(row.granted_by.as_deref(), Some("granted by you"));

        apply_revoke(&mut ws, OWNER, grant.id.as_str()).expect("the owner revokes it");
        assert!(
            !listed_permissions(&ws, &ChatCatalogue, OWNER)
                .iter()
                .any(|row| row.id == grant.id.as_str()),
            "revoked, it lists no more"
        );
    }
}

#[cfg(test)]
mod default_app_authority_tests {
    use super::*;
    use crate::desktop::authority::WorkspaceAuthority as _;
    use tilo_desktop_model::WorkspaceId;

    fn candidate(id: &str) -> desktop_policy::AppCandidate {
        desktop_policy::AppCandidate {
            package_id: id.into(),
            display_name: id.into(),
            resources: [desktop_policy::ResourceKind::extension("pdf").unwrap()].into(),
            secondary: false,
        }
    }

    #[test]
    fn default_app_authority_is_separately_authorized_and_cycles_inheritance_first() {
        {
            let candidates = [candidate("so.tilo.viewer"), candidate("so.tilo.other")];
            let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
            let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
            let mut ws = Workspace::new(WorkspaceId::from_u128(7), "Docs");
            ws.policy.set_owner("owner");

            assert!(
                ws.set_default_app_policy_as(
                    "member",
                    pdf.clone(),
                    desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_policy_as(
                "owner",
                pdf.clone(),
                desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                &resolver,
            )
            .unwrap();

            assert!(
                ws.set_default_app_choice_as(
                    "owner",
                    "member",
                    pdf.clone(),
                    desktop_policy::UserWorkspaceChoice::PersonalDefault,
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_choice_as(
                "member",
                "member",
                pdf.clone(),
                desktop_policy::UserWorkspaceChoice::App("so.tilo.other".into()),
                &resolver,
            )
            .unwrap();
            assert_eq!(
                ws.default_apps.user_choices["member"][&pdf],
                desktop_policy::UserWorkspaceChoice::App("so.tilo.other".into())
            );
        }

        {
            let candidates = [candidate("so.tilo.viewer")];
            let pdf = desktop_policy::ResourceKind::extension("pdf").unwrap();
            assert_eq!(
                next_workspace_default(None, &pdf, &candidates),
                desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into())
            );
            assert_eq!(
                next_user_default(None, &pdf, &candidates),
                desktop_policy::UserWorkspaceChoice::PersonalDefault
            );
        }
    }
}
