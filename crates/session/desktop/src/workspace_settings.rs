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

use crate::i18n::{t, t_args};
use tilo_action_policy::Refusal;
use tilo_sort::CompactSort as _;

use collab_types::caps::{Access, CapOverride, Effect, Expiry, Principal, Scope, Subject};
use collab_types::role::Role as CapRole;
use tilo_desktop_model::{UserId, Workspace};

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

/// The host's application catalogue, as workspace settings name apps and offer them as default
/// openers.
pub trait AppDirectory {
    /// The name a person knows `app_id` by, when the host knows the app.
    fn app_display_name(&self, app_id: &str) -> Option<String>;
    /// Applications that can open a kind of resource, for the Apps tab.
    fn default_app_candidates(&self) -> Vec<desktop_policy::AppCandidate> {
        Vec::new()
    }
    /// The name a person knows the resource at `address` by, as the resource itself publishes it (a
    /// page or an issue by its title), when the host has it.
    fn resource_label(&self, _address: &str) -> Option<String> {
        None
    }
    /// The workspace's resources the host knows, each with the app that publishes it: what a grant
    /// may name.
    fn resources(&self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The workspace's resources the host knows, each by the title its app publishes for it: what a
    /// person picks an existing resource from.
    fn titled_resources(&self) -> Vec<TitledResource> {
        Vec::new()
    }
}

/// A resource of the workspace by the title its app publishes for it, as a search finds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TitledResource {
    /// The app that keeps it.
    pub app: String,
    /// Its kind: an item's (`notes.notebook.pages`), else the resource's own.
    pub kind: String,
    /// Its id in that app.
    pub resource: String,
    pub title: String,
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

pub use tilo_desktop_model::{APP_ACTION_GRANT_ACTION_ID, APP_ACTION_REVOKE_ACTION_ID};

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

/// The catalogue message for a device-scoped capability, or `None` for a device the catalogue has
/// no name for (shown by its id).
fn device_scope_key(device: &str) -> Option<&'static str> {
    match device {
        "camera" => Some("workspace-settings-scope-camera"),
        "microphone" => Some("workspace-settings-scope-microphone"),
        "screencast" => Some("workspace-settings-scope-screencast"),
        "remote-desktop" => Some("workspace-settings-scope-remote-desktop"),
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
    let (key, args): (&str, Vec<(&str, ui::i18n::ArgValue)>) = match scope {
        Scope::FsPath(path) => fs_scope_phrase(path),
        Scope::FsRecord(record) => (
            "workspace-settings-scope-record",
            vec![("id", short_id(record).into())],
        ),
        Scope::Device(device) => match device_scope_key(device) {
            Some(key) => (key, Vec::new()),
            None => return device.clone(),
        },
        Scope::NetHost(host) if host == "*" => ("workspace-settings-scope-any-host", Vec::new()),
        Scope::NetHost(host) => ("workspace-settings-scope-host", vec![("host", host.into())]),
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
fn fs_scope_phrase(path: &str) -> (&'static str, Vec<(&'static str, ui::i18n::ArgValue)>) {
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
/// outside effects ask first". Where it says "everything", the grant covers every kind.
pub fn grant_allows(grant: &tilo_action_policy::Grant, apps: &dyn AppDirectory) -> String {
    grant_set_sentence(std::slice::from_ref(grant), apps)
}

/// A grant set as people and agents talk about it: one sentence per grant — where it applies, then
/// what it answers for each kind of action it covers — from the most specific node to the least. An
/// empty set grants nothing.
pub fn grant_set_sentence(grants: &[tilo_action_policy::Grant], apps: &dyn AppDirectory) -> String {
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
    tilo_action_policy::sentences(grants, &Named(apps))
}

/// Where a grant applies, named the way people know the workspace's parts: the app by its name, a
/// resource or item by its own, then the rest of the path.
pub fn node_phrase(node: &tilo_action_policy::TreePath, apps: &dyn AppDirectory) -> String {
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
    let below_workspace = node.within_workspace();
    if below_workspace.is_root() {
        return t("workspace-settings-scope-workspace");
    }
    tilo_action_policy::describe_node(&below_workspace, &Named(apps))
}

pub fn expiry_label(expiry: Expiry) -> String {
    t(match expiry {
        Expiry::AfterUse => "workspace-settings-expiry-once",
        Expiry::EndOfSession => "workspace-settings-expiry-session",
        Expiry::AtUnix(_) => "workspace-settings-expiry-until",
        Expiry::Never => "workspace-settings-expiry-always",
    })
}

/// Who granted a capability, as a user-facing phrase, or `None` for a capability with no recorded
/// grantor. People and agents are named as the People pane names them; the viewer is "you".
pub fn granted_by_label(cap: &CapOverride, ws: &Workspace, local_user: &str) -> Option<String> {
    cap.granted_by.as_ref().map(|p| match p {
        Principal::Person(id) | Principal::Agent(id) if id == local_user => {
            t("workspace-settings-granted-by-you")
        }
        Principal::Person(id) => t_args(
            "workspace-settings-granted-by-person",
            &[("id", member_name_or_short_id(ws, id).into())],
        ),
        Principal::App(id) => t_args("workspace-settings-granted-by-app", &[("app", id.into())]),
        Principal::Agent(id) => t_args(
            "workspace-settings-granted-by-agent",
            &[("id", member_name_or_short_id(ws, id).into())],
        ),
        Principal::Token(id) => t_args(
            "workspace-settings-granted-by-token",
            &[("id", short_id(id).into())],
        ),
        Principal::Workspace(id) => t_args(
            "workspace-settings-granted-by-workspace",
            &[("id", short_id(id).into())],
        ),
    })
}

/// How long a grant lives, as the composer's lifetimes name it.
pub fn lifetime_label(lifetime: &tilo_action_policy::Lifetime) -> String {
    use tilo_action_policy::Lifetime;
    t(match lifetime {
        Lifetime::Once => "workspace-settings-expiry-once",
        Lifetime::Session(_) => "workspace-settings-expiry-session",
        Lifetime::Until(_) => "workspace-settings-expiry-until",
        Lifetime::UntilRevoked => "workspace-settings-expiry-always",
    })
}

/// Who gave a grant, as a user-facing phrase; the viewer is "you".
pub fn grant_given_by_label(
    grant: &tilo_action_policy::Grant,
    ws: &Workspace,
    local_user: &str,
) -> Option<String> {
    use tilo_action_policy::Subject as Who;
    match &grant.granted_by {
        Who::Person(id) | Who::Agent(id) if id == local_user => {
            Some(t("workspace-settings-granted-by-you"))
        }
        Who::Person(id) => Some(t_args(
            "workspace-settings-granted-by-person",
            &[("id", member_name_or_short_id(ws, id).into())],
        )),
        Who::App(id) => Some(t_args(
            "workspace-settings-granted-by-app",
            &[("app", id.into())],
        )),
        Who::Agent(id) => Some(t_args(
            "workspace-settings-granted-by-agent",
            &[("id", member_name_or_short_id(ws, id).into())],
        )),
        Who::Role(_) | Who::Task(_) | Who::Shortcut(_) | Who::Bot(_) | Who::ForeignDevice => None,
    }
}

// Grant composer

/// A subject the grant composer can target, and the subject axis the Permissions list groups by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SubjectKind {
    App,
    /// An app granted another app's action: its own section, so what apps may do to each other is
    /// never lost among device and file access.
    AppAction,
    Member,
    Agent,
    Role,
    /// Bearer token, workspace, or the work on a resource (an issue's runs, a shortcut) — surfaced
    /// so no live capability is invisible in the panel, even though the composer can't mint one.
    Other,
}

impl SubjectKind {
    pub fn section_label(self) -> String {
        t(match self {
            SubjectKind::App => "workspace-settings-section-apps",
            SubjectKind::AppAction => "workspace-settings-section-app-actions",
            SubjectKind::Member => "workspace-settings-section-people",
            SubjectKind::Agent => "workspace-settings-section-agents",
            SubjectKind::Role => "workspace-settings-section-roles",
            SubjectKind::Other => "workspace-settings-section-other",
        })
    }
}

/// A resolved subject: what kind of thing it is and how it is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSubject {
    pub kind: SubjectKind,
    pub id: String,
}

impl GrantSubject {
    /// Who a sandbox capability for this subject is held by.
    pub fn to_subject(&self) -> Option<Subject> {
        match self.kind {
            SubjectKind::App | SubjectKind::AppAction => Some(Subject::app(self.id.clone())),
            SubjectKind::Member => Some(Subject::person(self.id.clone())),
            SubjectKind::Agent => Some(Subject::agent(self.id.clone())),
            // A role-wide grant is a policy decision, not a per-workspace click.
            SubjectKind::Role | SubjectKind::Other => None,
        }
    }

    pub fn display(&self, ws: &Workspace, local_user: &str, apps: &dyn AppDirectory) -> String {
        match self.kind {
            SubjectKind::Member | SubjectKind::Agent => person_name(ws, local_user, &self.id),
            SubjectKind::App | SubjectKind::AppAction => app_display_name(apps, &self.id),
            // A subject that is a resource's work is named as the resource.
            _ => apps
                .resource_label(&self.id)
                .unwrap_or_else(|| self.id.clone()),
        }
    }
}

/// What the composer's scope picker maps to. Kept as a small enum rather than a pre-built `Scope`
/// so the table can stay `const` .
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantTarget {
    Device(&'static str),
    AnyHost,
    WorkspaceFiles,
}

/// One entry in the composer's "what does this allow" picker. Every entry produces a capability an
/// enforcement point actually reads (devices gate the portal, `NetHost` gates egress, `FsPath`
/// gates the sandbox binds and the filesystem, `Workspace` gates administration).
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
        key: "camera",
        label_key: "workspace-settings-grant-camera",
        target: GrantTarget::Device("camera"),
        access: Access::Read,
    },
    GrantChoice {
        key: "microphone",
        label_key: "workspace-settings-grant-microphone",
        target: GrantTarget::Device("microphone"),
        access: Access::Read,
    },
    GrantChoice {
        key: "screencast",
        label_key: "workspace-settings-grant-screencast",
        target: GrantTarget::Device("screencast"),
        access: Access::Read,
    },
    GrantChoice {
        key: "remote-control",
        label_key: "workspace-settings-grant-remote-control",
        target: GrantTarget::Device("remote-desktop"),
        access: Access::Write,
    },
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

pub const EXPIRY_CHOICES: [Expiry; 3] = [Expiry::Never, Expiry::EndOfSession, Expiry::AfterUse];

/// The in-flight grant the Permissions tab's composer is composing. Holds indices rather than
/// resolved values so a subject leaving the workspace can't leave a dangling reference —
/// [`grant_subjects`] is re-derived every frame and the index taken modulo its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GrantDraft {
    pub subject_index: usize,
    pub choice_index: usize,
    pub expiry: Expiry,
}

impl Default for GrantDraft {
    fn default() -> Self {
        Self {
            subject_index: 0,
            choice_index: 0,
            expiry: Expiry::EndOfSession,
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

/// What a subject of the composer, a permission and an expiry are named by where a view asks for
/// the grant: `member:…` , `agent:…` or `app:…` , the permission's key, and `never` , `session` or
/// `once` .
#[must_use]
pub fn grant_subject_key(entry: &GrantSubject) -> Option<String> {
    let kind = match entry.kind {
        SubjectKind::Member => "member",
        SubjectKind::Agent => "agent",
        SubjectKind::App | SubjectKind::AppAction => "app",
        SubjectKind::Role | SubjectKind::Other => return None,
    };
    Some(format!("{kind}:{}", entry.id))
}

/// The expiry's key, as [`GrantComposer`] names it.
#[must_use]
pub fn expiry_key(expiry: Expiry) -> &'static str {
    match expiry {
        Expiry::Never => "never",
        Expiry::EndOfSession | Expiry::AtUnix(_) => "session",
        Expiry::AfterUse => "once",
    }
}

/// The composer's permissions as (key, label), in picker order.
#[must_use]
pub fn grant_choices() -> Vec<(&'static str, String)> {
    GRANT_CHOICES
        .iter()
        .map(|choice| (choice.key, choice.label()))
        .collect()
}

/// The composer's expiries as (key, label), in picker order.
#[must_use]
pub fn expiry_choices() -> Vec<(&'static str, String)> {
    EXPIRY_CHOICES
        .iter()
        .map(|expiry| (expiry_key(*expiry), expiry_label(*expiry)))
        .collect()
}

/// The draft a view asked for: the subject `subject` (`member:…`, `agent:…` or `app:…` ), the
/// permission `permission` and the expiry `expiry` , as the composer stands. `None` when the
/// workspace holds no such subject, so a grant for someone who left is never made for someone else.
#[must_use]
pub fn draft_for(
    ws: &Workspace,
    subject: &str,
    permission: &str,
    expiry: &str,
) -> Option<GrantDraft> {
    let subject_index = grant_subjects(ws)
        .iter()
        .position(|entry| grant_subject_key(entry).as_deref() == Some(subject))?;
    let choice_index = GRANT_CHOICES
        .iter()
        .position(|choice| choice.key == permission)?;
    let expiry = EXPIRY_CHOICES
        .iter()
        .copied()
        .find(|candidate| expiry_key(*candidate) == expiry)
        .unwrap_or(Expiry::EndOfSession);
    Some(GrantDraft {
        subject_index,
        choice_index,
        expiry,
    })
}

/// The subjects the composer can target, in picker order: the workspace's people first, then
/// admitted agents, then every app running in the workspace or already holding a capability —
/// without running apps, a host could only grant to an app already granted something.
pub fn grant_subjects(ws: &Workspace) -> Vec<GrantSubject> {
    let mut out: Vec<GrantSubject> = ws
        .roster()
        .into_iter()
        .map(|(user, _)| GrantSubject {
            kind: SubjectKind::Member,
            id: user,
        })
        .collect();

    out.extend(
        ws.agent_members_at(tilo_clock::unix_now())
            .into_iter()
            .map(|(agent, _)| GrantSubject {
                kind: SubjectKind::Agent,
                id: agent,
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
        if let Subject::Principal(Principal::App(app)) = &cap.subject {
            apps.push(app.clone());
        }
    }
    for grant in &ws.policy.grants {
        if let tilo_action_policy::Subject::App(app) = &grant.subject
            && !tilo_action_policy::roles::is_make_up(grant)
        {
            apps.push(app.clone());
        }
    }
    apps.compact_sort();
    apps.dedup();
    out.extend(apps.into_iter().map(|id| GrantSubject {
        kind: SubjectKind::App,
        id,
    }));
    out
}

/// The capability `draft` names against `ws` , or `None` when the draft has no addressable subject
/// (an empty workspace) or names a subject this panel cannot grant to.
pub fn draft_capability(ws: &Workspace, draft: &GrantDraft) -> Option<CapOverride> {
    let subjects = grant_subjects(ws);
    if subjects.is_empty() {
        return None;
    }
    let idx = draft.subject_index.checked_rem(subjects.len()).unwrap_or(0);
    let subject_entry = subjects.get(idx)?;
    let choice = draft.choice();
    let scope = match choice.target {
        GrantTarget::Device(d) => Scope::Device(d.to_string()),
        GrantTarget::AnyHost => Scope::NetHost("*".to_string()),
        GrantTarget::WorkspaceFiles => Scope::FsPath(WORKSPACE_FILES_PATH.to_string()),
    };
    let subject = subject_entry.to_subject()?;
    Some(CapOverride::grant(subject, scope, choice.access).expiring(draft.expiry))
}

/// What a panel mutation changed, for the caller that applies it to the desktop. Empty means there
/// was nothing to change.
pub use tilo_desktop_model::grants::PolicyEdit;

/// Grant what `draft` names, as `actor`, through the workspace's governance:
/// the same check every door and the workspace's host make.
///
/// # Errors
/// The workspace refused the grant, saying why.
pub fn apply_grant_draft(
    ws: &mut Workspace,
    actor: &UserId,
    draft: &GrantDraft,
) -> Result<PolicyEdit, Refusal> {
    let now = tilo_clock::unix_now();
    match draft_capability(ws, draft) {
        Some(cap) => ws.grant_sandbox_as_at(actor, cap, now),
        None => Ok(PolicyEdit::default()),
    }
}

/// Revoke the grant or sandbox capability with id `id`, as `actor`.
///
/// # Errors
/// The workspace refused the revocation, saying why.
pub fn apply_revoke(ws: &mut Workspace, actor: &UserId, id: &str) -> Result<PolicyEdit, Refusal> {
    let now = tilo_clock::unix_now();
    if ws.sandbox.iter().any(|cap| cap.id == id) {
        return ws.revoke_sandbox_as_at(actor, id, now);
    }
    ws.govern_all_at(
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
    actor: &UserId,
    member: &UserId,
    role: CapRole,
) -> Result<PolicyEdit, Refusal> {
    if role == CapRole::Owner {
        return Err(Refusal::OwnerRole);
    }
    if actor == member {
        return Err(Refusal::NotPermitted(crate::i18n::t(
            "workspace-settings-refused-own-role",
        )));
    }
    if ws.is_owner(member) {
        return Err(Refusal::Owner);
    }
    let change = if ws.policy.members.contains_key(member) {
        tilo_action_policy::Change::SetRole {
            person: member.clone(),
            role,
        }
    } else {
        tilo_action_policy::Change::AddMember {
            person: member.clone(),
            role,
        }
    };
    ws.govern_all_at(actor, vec![change], tilo_clock::unix_now())
}

/// Remove `member` and every grant naming them, as `actor`.
///
/// # Errors
/// The workspace refused the removal, saying why.
pub fn remove_member(
    ws: &mut Workspace,
    actor: &UserId,
    member: &UserId,
) -> Result<PolicyEdit, Refusal> {
    // The owner is the workspace's root of trust; removing them would leave a workspace nobody
    // governs.
    if ws.is_owner(member) {
        return Err(Refusal::Owner);
    }
    if actor == member {
        return Err(Refusal::NotPermitted(crate::i18n::t(
            "workspace-settings-refused-own-membership",
        )));
    }
    let now = tilo_clock::unix_now();
    let mut edit = ws.govern_all_at(
        actor,
        vec![tilo_action_policy::Change::RemoveMember {
            person: member.clone(),
        }],
        now,
    )?;
    // What the sandbox let them reach goes too.
    let held: Vec<String> = ws
        .sandbox
        .iter()
        .filter(|cap| subject_is_person(&cap.subject, member))
        .map(|cap| cap.id.clone())
        .collect();
    for id in held {
        if let Ok(revoked) = ws.revoke_sandbox_as_at(actor, &id, now) {
            edit = edit.and(revoked);
        }
    }
    // Off the roster too, but only if the removal happened.
    if !edit.is_empty() {
        ws.collaborators.retain(|c| c.id != member.as_str());
    }
    Ok(edit)
}

fn subject_is_person(subject: &Subject, user: &UserId) -> bool {
    match subject {
        Subject::Principal(Principal::Person(id)) => id == user,
        Subject::User(key) => Principal::person_from_key(*key).id() == user.as_str(),
        Subject::Principal(_) => false,
    }
}

/// Revoke every grant naming `agent` , as `actor` .
pub fn remove_agent(ws: &mut Workspace, actor: &UserId, agent: &str) -> PolicyEdit {
    let subject = tilo_action_policy::Subject::agent(agent);
    let revokes = ws
        .policy
        .grants
        .iter()
        .filter(|grant| grant.subject == subject)
        .map(|grant| tilo_action_policy::Change::Revoke(grant.id.clone()))
        .collect();
    ws.govern_all_at(actor, revokes, tilo_clock::unix_now())
        .unwrap_or_default()
}

// Members pane

/// The roster in a stable display order: owner first, then everyone
/// else by id. [`Workspace::roster`]'s own order follows list position,
/// which moves a row when a role change rewrites its capability.
fn ordered_roster(ws: &Workspace) -> Vec<(UserId, CapRole)> {
    let mut rows = ws.roster();
    let owner = ws.owner().cloned();
    rows.compact_sort_by(|a, b| {
        let a_owner = Some(&a.0) == owner.as_ref();
        let b_owner = Some(&b.0) == owner.as_ref();
        b_owner.cmp(&a_owner).then_with(|| a.0.cmp(&b.0))
    });
    rows
}

/// A member by name, numbered when another member here shares it
/// ([`ui::presence::ParticipantLabels`]), or by a shortened id.
fn member_name_or_short_id(ws: &Workspace, user: &str) -> String {
    ws.collaborators
        .iter()
        .find(|person| person.id == user && !person.name.trim().is_empty())
        .map_or_else(
            || short_id(user),
            |person| {
                ui::presence::ParticipantLabels::by_joining(ws.collaborators.iter().map(|person| {
                    (
                        person.id.as_str(),
                        person.name.as_str(),
                        ws.policy.joined_at(&person.id),
                    )
                }))
                .label(user, person.name.trim())
            },
        )
}

/// A member as the People pane names them: as the roster does, the viewer by their own name; only
/// someone the roster has never seen falls back to the workspace's record of them.
#[must_use]
pub fn member_row_name(
    names: &crate::collaborators::PeopleNames,
    ws: &Workspace,
    local_user: &str,
    id: &str,
) -> String {
    names
        .get(id)
        .map_or_else(|| person_name(ws, local_user, id), str::to_owned)
}

/// A person or agent as every workspace-settings pane names them: their collaborator name, the
/// viewer included; the viewer by the name this device knows them by when the workspace has none;
/// else a shortened id.
fn person_name(ws: &Workspace, local_user: &str, id: &str) -> String {
    let known = ws
        .collaborators
        .iter()
        .any(|person| person.id == id && !person.name.trim().is_empty());
    if id == local_user
        && !known
        && let Some(name) = ui::local_identity::bound()
            .map(|identity| identity.display_name.trim().to_owned())
            .filter(|name| !name.is_empty())
    {
        return name;
    }
    member_name_or_short_id(ws, id)
}

/// What the People tab says about who may change a member's role.
pub fn members_authority_note(is_owner: bool) -> Option<String> {
    (!is_owner).then(|| t("workspace-settings-owner-only"))
}

/// What a bot says under its name: whether it runs yet, and from which version. One fact per line,
/// never joined.
pub fn bot_detail(bot: &tilo_desktop_model::WorkspaceBot) -> String {
    if bot.installation.is_none() {
        t("workspace-settings-bot-starting")
    } else if bot.paused {
        t("workspace-settings-bot-paused")
    } else if let Some(version) = &bot.version {
        t_args(
            "workspace-settings-bot-version",
            &[("version", version.as_str().into())],
        )
    } else {
        t("workspace-settings-bot-running")
    }
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

/// The section a sandbox capability is listed in: its subject's.
fn classify_capability(cap: &CapOverride) -> GrantSubject {
    classify_subject(&cap.subject)
}

fn classify_subject(subject: &Subject) -> GrantSubject {
    match subject {
        Subject::Principal(Principal::App(id)) => GrantSubject {
            kind: SubjectKind::App,
            id: id.clone(),
        },
        Subject::Principal(Principal::Person(id)) => GrantSubject {
            kind: SubjectKind::Member,
            id: id.clone(),
        },
        Subject::User(key) => GrantSubject {
            kind: SubjectKind::Member,
            id: Principal::person_from_key(*key).id().to_string(),
        },
        Subject::Principal(Principal::Agent(id)) => GrantSubject {
            kind: SubjectKind::Agent,
            id: id.clone(),
        },
        Subject::Principal(Principal::Token(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: t_args(
                "workspace-settings-subject-token",
                &[("id", short_id(id).into())],
            ),
        },
        Subject::Principal(Principal::Workspace(id)) => GrantSubject {
            kind: SubjectKind::Other,
            id: t_args(
                "workspace-settings-subject-workspace",
                &[("id", short_id(id).into())],
            ),
        },
    }
}

/// The section a grant is listed in: its subject's, except that an app granted another app's action
/// is listed under App actions.
fn classify_grant(grant: &tilo_action_policy::Grant) -> GrantSubject {
    use tilo_action_policy::Subject as Who;
    let names_action = grant
        .node
        .id_at(tilo_action_policy::Level::Action)
        .is_some();
    match &grant.subject {
        Who::App(id) => GrantSubject {
            kind: if names_action {
                SubjectKind::AppAction
            } else {
                SubjectKind::App
            },
            id: id.clone(),
        },
        Who::Person(id) => GrantSubject {
            kind: SubjectKind::Member,
            id: id.clone(),
        },
        Who::Agent(id) => GrantSubject {
            kind: SubjectKind::Agent,
            id: id.clone(),
        },
        Who::Role(role) => GrantSubject {
            kind: SubjectKind::Role,
            id: t_args(
                "workspace-settings-subject-role",
                &[("role", role_label(*role).into())],
            ),
        },
        Who::Task(id) | Who::Shortcut(id) => GrantSubject {
            kind: SubjectKind::Other,
            id: id.clone(),
        },
        Who::Bot(id) => GrantSubject {
            kind: SubjectKind::Other,
            id: t_args(
                "workspace-settings-subject-bot",
                &[("id", short_id(id).into())],
            ),
        },
        Who::ForeignDevice => GrantSubject {
            kind: SubjectKind::Other,
            id: t("workspace-settings-subject-foreign-device"),
        },
    }
}

/// What the Permissions pane lists, grouped by subject kind, then subject,
/// then id: every sandbox capability granted, and every grant but the
/// role presets (the People tab is roles) and an agent's admission (the
/// Agents tab), or it would be revocable from neither tab.
fn listed_capabilities(ws: &Workspace) -> Vec<(GrantSubject, Listed<'_>)> {
    let mut out: Vec<(GrantSubject, Listed<'_>)> = ws
        .sandbox
        .iter()
        .filter(|cap| cap.effect == Effect::Grant)
        .map(|cap| (classify_capability(cap), Listed::Sandbox(cap)))
        .collect();
    out.extend(
        ws.policy
            .grants
            .iter()
            .filter(|grant| {
                let preset = matches!(grant.subject, tilo_action_policy::Subject::Role(_))
                    && grant.id.as_str().starts_with("role:");
                let admission = matches!(grant.subject, tilo_action_policy::Subject::Agent(_))
                    && grant.id.as_str().starts_with("agent-member:");
                let make_up = tilo_action_policy::roles::is_make_up(grant);
                !preset && !admission && !make_up
            })
            .map(|grant| (classify_grant(grant), Listed::Grant(grant))),
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
                    expiry_label(cap.expiry),
                    granted_by_label(cap, ws, local_user),
                ),
                Listed::Grant(grant) => (
                    grant_allows(grant, apps),
                    lifetime_label(&grant.lifetime),
                    grant_given_by_label(grant, ws, local_user),
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
    use crate::authority::WorkspaceAuthority as _;
    use tilo_action_policy::{ActionKind, Answer, Change, Grant, KindSet, roles};
    use tilo_desktop_model::WorkspaceSlot;

    const OWNER: &str = "owner-user";
    const PEER: &str = "peer-user";

    fn ws_with_two_members() -> Workspace {
        let mut ws = Workspace::new(WorkspaceSlot::from(7), "members-test");
        ws.set_owner(OWNER);
        assert!(
            set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Viewer
            )
            .is_ok_and(|edit| !edit.is_empty()),
            "owner may admit a member"
        );
        ws
    }

    /// Whether `who` may change the workspace's content now.
    fn may_write(ws: &Workspace, who: &str) -> bool {
        ws.answer_at(
            who,
            &ws.node(),
            ActionKind::Undoable,
            tilo_clock::unix_now(),
        ) == Answer::Allow
    }

    #[test]
    fn people_are_named_as_the_roster_names_them() {
        use crate::collaborators::{PeopleNames, collaborator_roster_rows_named};
        let owner_key = "fa4834147f".repeat(6) + "0123";
        let mut ws = Workspace::new(WorkspaceSlot::from(9), "Studio");
        ws.set_owner(owner_key.as_str());
        assert!(
            set_member_role(&mut ws, &owner_key, &"ada".to_string(), CapRole::Editor)
                .is_ok_and(|edit| !edit.is_empty())
        );
        // The roster knows Ada from presence; the workspace's own record of her has no name.
        let desktop = tilo_desktop_model::DesktopTree {
            active: ws.slot,
            workspaces: vec![ws.clone()],
            collaborators: vec![tilo_desktop_model::Collaborator::new("ada", "Ada")],
            ..tilo_desktop_model::DesktopTree::default()
        };
        let names = PeopleNames::of(&desktop, &owner_key, Some("Robin"));
        let (roster, own) = collaborator_roster_rows_named(&desktop, &owner_key, Some("Robin"));
        let roster_ada = roster
            .iter()
            .find(|row| row.id == "ada")
            .map(|row| row.label.clone());
        assert_eq!(
            Some(member_row_name(&names, &ws, &owner_key, "ada")),
            roster_ada,
            "the same name, cased the same, as the roster"
        );
        let owner_name = member_row_name(&names, &ws, &owner_key, &owner_key);
        assert_eq!(Some(owner_name.clone()), own);
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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn member_roles_come_from_the_roster_and_change_only_within_owner_authority() {
        {
            let mut ws = ws_with_two_members();
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            let edit = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Editor,
            )
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
            set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Editor,
            )
            .expect("the owner may set a role");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Editor));
            set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Viewer,
            )
            .expect("the owner may set a role");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            assert!(
                !may_write(&ws, PEER),
                "a demoted member must stop being able to write, not merely stop reading as Editor"
            );
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(
                &mut ws,
                &PEER.to_string(),
                &PEER.to_string(),
                CapRole::Owner,
            );
            assert!(
                refusal.is_err(),
                "a viewer must not be able to promote itself"
            );
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &OWNER.to_string(),
                CapRole::Viewer,
            );
            assert!(refusal.is_err(), "nobody changes their own role");
            assert_eq!(ws.role_of(OWNER), Some(CapRole::Owner));
        }

        {
            let mut ws = ws_with_two_members();
            let refusal = set_member_role(
                &mut ws,
                &OWNER.to_string(),
                &PEER.to_string(),
                CapRole::Owner,
            );
            assert!(refusal.is_err(), "ownership is not handed out");
            assert_eq!(ws.role_of(PEER), Some(CapRole::Viewer));
            assert_eq!(ws.owner().map(String::as_str), Some(OWNER));
        }

        {
            let mut ws = ws_with_two_members();
            let admin = Grant::new(
                "peer-administers",
                tilo_action_policy::Subject::person(PEER),
                roles::workspace_resource(&ws.policy.workspace),
                KindSet::ALL,
                Answer::Allow,
                tilo_action_policy::Subject::person(OWNER),
            );
            ws.govern_as_at(OWNER, Change::Grant(admin), tilo_clock::unix_now())
                .expect("owner delegates administration");
            let before = ws.policy.clone();

            assert!(
                set_member_role(
                    &mut ws,
                    &PEER.to_string(),
                    &PEER.to_string(),
                    CapRole::Viewer,
                )
                .is_err()
            );
            assert!(remove_member(&mut ws, &PEER.to_string(), &PEER.to_string()).is_err());
            assert_eq!(ws.policy, before);
            assert!(ws.may_administer(&PEER.to_string()));
        }
    }

    #[test]
    fn member_removal_takes_back_what_the_member_held() {
        let mut ws = ws_with_two_members();
        let cap = CapOverride::grant(
            Subject::person(PEER),
            Scope::FsPath("/var/workspace".into()),
            Access::Write,
        );
        ws.grant_as(&OWNER.to_string(), cap).expect("owner grants");
        let notes = Grant::new(
            "peer-notes",
            tilo_action_policy::Subject::person(PEER),
            ws.node().app("so.tilo.notes"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            tilo_action_policy::Subject::person(OWNER),
        );
        ws.govern_as_at(OWNER, Change::Grant(notes), tilo_clock::unix_now())
            .expect("owner shares an app");

        let edit = remove_member(&mut ws, &OWNER.to_string(), &PEER.to_string())
            .expect("the owner may remove a member");
        assert_eq!(edit.members, vec![PEER.to_string()]);
        assert_eq!(edit.revoked.len(), 1, "the app grant");
        assert_eq!(edit.sandbox_revoked.len(), 1, "the file grant");
        assert_eq!(ws.role_of(PEER), None);
        assert!(
            !ws.allows(
                &ws.actor_for_person(&PEER.to_string()),
                &collab_types::caps::ResourceRef::FsPath("/var/workspace/notes".into()),
                Access::Write
            ),
            "removing a member must revoke what they held, not just hide them"
        );

        let refusal = remove_member(&mut ws, &OWNER.to_string(), &OWNER.to_string());
        assert!(refusal.is_err(), "the owner cannot be removed");
        assert_eq!(ws.role_of(OWNER), Some(CapRole::Owner));
    }
}

#[cfg(test)]
mod agents_tests {
    //! An agent is a permission subject like any other: what it may do reads back from the
    //! workspace's grants naming it, per subject.

    use super::*;
    use crate::authority::WorkspaceAuthority as _;
    use tilo_action_policy::{ActionKind, Answer, Change, Grant, KindSet};
    use tilo_desktop_model::WorkspaceSlot;

    const OWNER: &str = "owner-user";
    const AGENT: &str = "scribe-agent";

    fn ws_with_agent() -> Workspace {
        let mut ws = Workspace::new(WorkspaceSlot::from(17), "agents-test");
        ws.set_owner(OWNER);
        ws.grant(Grant::new(
            format!("agent-member:{AGENT}"),
            tilo_action_policy::Subject::agent(AGENT),
            ws.node(),
            KindSet::READ,
            Answer::Allow,
            tilo_action_policy::Subject::person(OWNER),
        ));
        let notes = Grant::new(
            "scribe-notes",
            tilo_action_policy::Subject::agent(AGENT),
            ws.node().app("so.tilo.notes"),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            tilo_action_policy::Subject::person(OWNER),
        );
        ws.govern_as_at(OWNER, Change::Grant(notes), tilo_clock::unix_now())
            .expect("owner grants scoped access");
        ws
    }

    fn agent_answer(ws: &Workspace, app: &str, kind: ActionKind) -> Answer {
        ws.subject_authority(tilo_action_policy::Subject::agent(AGENT))
            .decide(
                &ws.node().app(app),
                kind,
                &tilo_desktop_model::context_at(tilo_clock::unix_now()),
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
            let members = ws.agent_members_at(tilo_clock::unix_now());
            assert_eq!(members.len(), 1, "listed with the workspace's agents");
            assert_eq!(members[0].0, AGENT);
            assert_eq!(
                members[0].1,
                KindSet::READ,
                "the workspace grant admits it; app grants are its own"
            );
        }

        {
            let mut ws = Workspace::new(WorkspaceSlot::from(18), "profile-test");
            ws.set_owner(OWNER);
            ws.grant(Grant::new(
                format!("agent-member:{AGENT}"),
                tilo_action_policy::Subject::agent(AGENT),
                ws.node(),
                KindSet::READ,
                Answer::Allow,
                tilo_action_policy::Subject::person(OWNER),
            ));
            assert_eq!(
                ws.agent_members_at(tilo_clock::unix_now()),
                vec![(AGENT.to_string(), KindSet::READ)]
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
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::agent(AGENT),
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
            let edit = remove_agent(&mut ws, &OWNER.to_string(), AGENT);
            assert_eq!(
                edit.revoked.len(),
                2,
                "its workspace grant and its app grant"
            );
            assert!(ws.agent_members_at(tilo_clock::unix_now()).is_empty());
            assert_eq!(
                agent_answer(&ws, "so.tilo.notes", ActionKind::Undoable),
                Answer::Deny
            );
        }

        {
            let mut ws = ws_with_agent();
            let before = ws.policy.clone();
            assert!(remove_agent(&mut ws, &"viewer".into(), AGENT).is_empty());
            assert_eq!(ws.policy, before);
        }

        {
            let ws = ws_with_agent();
            let subjects = grant_subjects(&ws);
            let index = subjects
                .iter()
                .position(|subject| subject.kind == SubjectKind::Agent && subject.id == AGENT)
                .expect("agent is offered in the permission composer");
            let draft = GrantDraft {
                subject_index: index,
                ..GrantDraft::default()
            };
            let Some(cap) = draft_capability(&ws, &draft) else {
                panic!("agent grant can be composed");
            };
            assert_eq!(cap.subject, Subject::agent(AGENT));
        }

        {
            let ws = ws_with_agent();
            let listed = listed_permissions(&ws, &NoAppDirectory, OWNER);
            assert_eq!(
                listed
                    .iter()
                    .filter(|row| row.section == SubjectKind::Agent.section_label())
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
    use crate::authority::WorkspaceAuthority as _;
    use collab_types::caps::ResourceRef;
    use tilo_desktop_model::{Capability, GRANT_ALWAYS, GRANT_SESSION, WorkspaceSlot};

    const OWNER: &str = "owner-user";
    /// Whoever is looking at the tab; never a grantor in these fixtures.
    const VIEWER: &str = "viewer-user";

    fn app_actor(ws: &Workspace, app: &str) -> collab_types::caps::Actor {
        collab_types::caps::Actor::app(app).in_workspace(ws.id().to_string())
    }

    fn ws_with_grants() -> Workspace {
        let mut ws = Workspace::new(WorkspaceSlot::from(3), "perm-test");
        ws.set_owner(OWNER);
        ws.grant_app(
            "firefox",
            &Capability::Camera {
                scope: GRANT_ALWAYS,
            },
        );
        ws.grant_app(
            "firefox",
            &Capability::Network {
                hosts: None,
                scope: GRANT_SESSION,
            },
        );
        ws
    }

    fn listed(ws: &Workspace) -> Vec<ListedPermission> {
        listed_permissions(ws, &NoAppDirectory, VIEWER)
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
                scope_label(&Scope::FsRecord(record.to_string()), &NoAppDirectory),
                "one shared file (b0d4e23f-c…)"
            );
            assert_eq!(
                capability_allows(
                    &CapOverride::grant(
                        Subject::Principal(Principal::App("files".to_string())),
                        Scope::FsRecord(record.to_string()),
                        Access::Read,
                    ),
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
                assert_eq!(row.section, SubjectKind::App.section_label());
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
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::person("peer-user"),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                ),
            )
            .expect("owner grants");
            ws.govern_as_at(
                OWNER,
                tilo_action_policy::Change::Grant(tilo_action_policy::Grant::new(
                    "editors-camera-app",
                    tilo_action_policy::Subject::Role(CapRole::Editor),
                    ws.node().app("so.tilo.camera"),
                    tilo_action_policy::KindSet::READ,
                    tilo_action_policy::Answer::Allow,
                    tilo_action_policy::Subject::person(OWNER),
                )),
                tilo_clock::unix_now(),
            )
            .expect("owner grants");
            let rows = listed(&ws);
            let kinds: Vec<&str> = rows.iter().map(|r| r.section.as_str()).collect();
            for expected in [
                SubjectKind::App.section_label(),
                SubjectKind::Member.section_label(),
                SubjectKind::Role.section_label(),
            ] {
                assert!(kinds.contains(&expected.as_str()), "{kinds:?}");
            }
            let mut sorted = kinds.clone();
            sorted.sort_unstable();
            assert_eq!(kinds, sorted, "rows are grouped by subject kind");
        }

        {
            // A 64-hex person key is what the model records; the row names the collaborator, and
            // the viewer's own grant reads "you".
            let ada = "ad".repeat(32);
            let bea = "be".repeat(32);
            let mut ws = Workspace::new(WorkspaceSlot::from(5), "names");
            ws.set_owner(&ada);
            ws.collaborators.push(tilo_desktop_model::Collaborator::new(
                ada.clone(),
                "Ada Lovelace",
            ));
            ws.collaborators
                .push(tilo_desktop_model::Collaborator::new(bea.clone(), "Bea"));
            ws.grant_as(
                &ada,
                CapOverride::grant(
                    Subject::person(bea.clone()),
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
            let mut ws = Workspace::new(WorkspaceSlot::from(4), "labels");
            ws.set_owner(OWNER);
            ws.grant_as(
                &OWNER.to_string(),
                CapOverride::grant(
                    Subject::app("terminal"),
                    Scope::FsPath("/var/workspace".into()),
                    Access::Write,
                )
                .expiring(Expiry::EndOfSession),
            )
            .expect("owner grants");
            let row = &listed(&ws)[0];
            assert_eq!(row.allows, "Edit workspace files");
            assert_eq!(row.expiry, "Session");
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
                Scope::Device("camera".into()),
                Scope::NetHost("*".into()),
                Scope::NetHost("example.com".into()),
                Scope::FsRecord("b0d4e23f-c7cb-4ded-a427-b32cf9da9341".into()),
            ];
            for scope in &scopes {
                for verb in [Access::Read, Access::Write] {
                    let cap = CapOverride::grant(Subject::app("a"), scope.clone(), verb);
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
                Subject::app("a"),
                Scope::FsPath(WORKSPACE_FILES_PATH.into()),
                Access::Write,
            );
            let sub = CapOverride::grant(
                Subject::app("a"),
                Scope::FsPath(format!("{WORKSPACE_FILES_PATH}/design")),
                Access::Write,
            );
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn granting_and_revoking_permissions_is_owner_only_and_enforced() {
        {
            let ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            assert_eq!(
                subjects[0],
                GrantSubject {
                    kind: SubjectKind::Member,
                    id: OWNER.to_string()
                },
                "people come first"
            );
            assert!(subjects.contains(&GrantSubject {
                kind: SubjectKind::App,
                id: "firefox".to_string()
            }));
        }

        {
            let mut ws = ws_with_grants();
            let subjects = grant_subjects(&ws);
            let firefox = subjects
                .iter()
                .position(|s| s.id == "firefox")
                .expect("firefox is a candidate subject");
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::Never,
            };
            while draft.choice().label() != "Microphone" {
                draft.cycle_choice();
            }
            assert!(!ws.allows(
                &app_actor(&ws, "firefox"),
                &ResourceRef::Device("microphone".into()),
                Access::Read
            ));

            let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft)
                .expect("the owner may grant");
            assert_eq!(edit.sandbox_granted.len(), 1);
            assert!(
                ws.allows(
                    &app_actor(&ws, "firefox"),
                    &ResourceRef::Device("microphone".into()),
                    Access::Read
                ),
                "the granted capability must start working, not merely start rendering"
            );
        }

        {
            let mut ws = ws_with_grants();
            let firefox = grant_subjects(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            // Re-granting an existing capability merges expiries, so pick something the app doesn't
            // already hold to observe this one.
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::AfterUse,
            };
            while draft.choice().label() != "Microphone" {
                draft.cycle_choice();
            }
            let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft)
                .expect("the owner may grant");
            let id = &edit.sandbox_granted[0];
            let cap = ws
                .sandbox
                .iter()
                .find(|c| &c.id == id)
                .expect("stored capability");
            assert_eq!(cap.expiry, Expiry::AfterUse);
            assert!(
                !cap.persists_across_restart(),
                "an after-use grant must not be written down as permanent"
            );
        }

        {
            // `grant_capability` keeps the longer expiry for the same (subject, scope, effect,
            // verb).
            let mut ws = ws_with_grants();
            let firefox = grant_subjects(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            let mut draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::AfterUse,
            };
            while draft.choice().label() != "Camera" {
                draft.cycle_choice();
            }
            apply_grant_draft(&mut ws, &OWNER.to_string(), &draft).expect("the owner may grant");
            let camera = ws
                .sandbox
                .iter()
                .filter(|c| matches!(&c.scope, Scope::Device(d) if d == "camera"))
                .collect::<Vec<_>>();
            assert_eq!(
                camera.len(),
                1,
                "the re-grant merges rather than duplicating"
            );
            assert_eq!(
                camera[0].expiry,
                Expiry::Never,
                "the existing permanent grant is not demoted to once-only"
            );
            let row = listed(&ws)
                .into_iter()
                .find(|r| r.allows.contains("camera"))
                .expect("camera row");
            assert_eq!(
                row.expiry, "Always",
                "the row must report the expiry that is stored, not the one that was asked for"
            );
        }

        {
            let mut ws = ws_with_grants();
            let firefox = grant_subjects(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            let draft = GrantDraft {
                subject_index: firefox,
                choice_index: 0,
                expiry: Expiry::Never,
            };
            let before = ws.sandbox.len();
            assert!(
                apply_grant_draft(&mut ws, &"someone-else".to_string(), &draft).is_err(),
                "grant_as must refuse a non-administrator"
            );
            assert_eq!(ws.sandbox.len(), before);
        }

        {
            let ws = ws_with_grants();
            let firefox = grant_subjects(&ws)
                .iter()
                .position(|s| s.id == "firefox")
                .unwrap();
            for (i, choice) in GRANT_CHOICES.iter().enumerate() {
                let draft = GrantDraft {
                    subject_index: firefox,
                    choice_index: i,
                    expiry: Expiry::Never,
                };
                let mut probe = ws.clone();
                let cap = draft_capability(&ws, &draft).expect("every choice resolves");
                assert_eq!(cap.effect, Effect::Grant);
                let resource = match &cap.scope {
                    Scope::Device(d) => ResourceRef::Device(d.clone()),
                    Scope::NetHost(h) => ResourceRef::NetHost(h.clone()),
                    Scope::FsPath(p) => ResourceRef::FsPath(p.clone()),
                    Scope::FsRecord(record) => {
                        panic!("unexpected scope in the grant table: {record:?}")
                    }
                };
                probe
                    .grant_as(&OWNER.to_string(), cap.clone())
                    .expect("owner grants");
                assert!(
                    probe.allows(&app_actor(&probe, "firefox"), &resource, cap.access),
                    "granting {:?} must make the decision function allow it",
                    choice.label()
                );
            }
        }

        {
            let mut ws = ws_with_grants();
            let now = tilo_clock::unix_now();
            let grant = tilo_action_policy::roles::app_authorship(
                &ws.policy.workspace,
                "firefox",
                tilo_action_policy::Subject::person(OWNER),
            );
            let id = grant.id.as_str().to_owned();
            ws.govern_as_at(OWNER, tilo_action_policy::Change::Grant(grant.clone()), now)
                .expect("owner grants");
            assert!(ws.app_is_author_at("firefox", now));
            let row = listed(&ws)
                .into_iter()
                .find(|r| r.id == id)
                .expect("the workspace-scoped app grant has a row");
            assert_eq!(row.section, SubjectKind::App.section_label());
            assert_eq!(row.allows, grant_allows(&grant, &NoAppDirectory));
            assert_eq!(
                apply_revoke(&mut ws, &OWNER.to_string(), &row.id)
                    .expect("the owner may revoke")
                    .revoked
                    .len(),
                1
            );
            assert!(!ws.app_is_author_at("firefox", now));
        }

        {
            let mut ws = ws_with_grants();
            let camera_row = listed(&ws)
                .into_iter()
                .find(|r| r.allows.contains("camera"))
                .expect("camera row");
            let edit = apply_revoke(&mut ws, &OWNER.to_string(), &camera_row.id)
                .expect("the owner may revoke");
            assert_eq!(edit.sandbox_revoked, vec![camera_row.id.clone()]);
            assert!(
                !ws.allows(
                    &app_actor(&ws, "firefox"),
                    &ResourceRef::Device("camera".into()),
                    Access::Read
                ),
                "the revoked capability must stop working, not merely stop rendering"
            );
            assert_eq!(listed(&ws).len(), 1);
        }

        {
            let mut ws = ws_with_grants();
            let target = listed(&ws)[0].id.clone();
            let refusal = apply_revoke(&mut ws, &"someone-else".to_string(), &target);
            assert!(refusal.is_err(), "only an administrator may revoke");
            assert_eq!(listed(&ws).len(), 2);
        }

        {
            let mut ws = ws_with_grants();
            let refusal = apply_revoke(&mut ws, &OWNER.to_string(), "cap-does-not-exist");
            assert!(refusal.is_err(), "there is no such grant to revoke");
            assert_eq!(ws.sandbox.len(), 2, "the two grants");
        }
    }

    #[test]
    fn a_resource_is_picked_by_the_title_its_app_publishes() {
        struct Titles;
        impl AppDirectory for Titles {
            fn app_display_name(&self, _app_id: &str) -> Option<String> {
                None
            }
            fn titled_resources(&self) -> Vec<TitledResource> {
                vec![TitledResource {
                    app: "so.tilo.notes".into(),
                    kind: "notes.notebook.pages".into(),
                    resource: "q3".into(),
                    title: "Q3 plan".into(),
                }]
            }
        }

        let mut ws = ws_with_grants();
        let draft = draft_for(&ws, "app:firefox", "microphone", "never").expect("a draft");
        let mut ws2 = ws.clone();
        apply_grant_draft(&mut ws2, &OWNER.to_string(), &draft).expect("owner grants");
        ws = ws2;
        let rows = listed_permissions(&ws, &Titles, VIEWER);
        assert!(
            rows.iter().all(|row| !row.allows.contains("q3")),
            "a printed permission names what it opens, never a bare id"
        );
    }

    #[test]
    fn a_grant_set_reads_back_as_the_grants_the_workspace_holds() {
        let mut ws = ws_with_grants();
        let draft = draft_for(&ws, "app:firefox", "microphone", "never").expect("a draft");
        let edit = apply_grant_draft(&mut ws, &OWNER.to_string(), &draft).expect("owner grants");
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

#[cfg(test)]
mod bot_detail_tests {
    use super::*;
    use tilo_desktop_model::WorkspaceBot;

    fn bot(version: Option<&str>) -> WorkspaceBot {
        WorkspaceBot {
            name: "Standup".into(),
            installation: Some("standup-installation".into()),
            device: Some("cd".repeat(32)),
            entry: Some("so.tilo.bots.standup".into()),
            version: version.map(str::to_owned),
            added_by: "owner-user".into(),
            requested: true,
            paused: false,
        }
    }

    #[test]
    fn a_bot_says_whether_it_runs_and_from_which_version() {
        assert_eq!(
            bot_detail(&bot(Some("1.2.0"))),
            t_args(
                "workspace-settings-bot-version",
                &[("version", "1.2.0".into())]
            )
        );
        let mut starting = bot(Some("1.2.0"));
        starting.installation = None;
        assert_eq!(bot_detail(&starting), t("workspace-settings-bot-starting"));
        let mut paused = bot(Some("1.2.0"));
        paused.paused = true;
        assert_eq!(bot_detail(&paused), t("workspace-settings-bot-paused"));
        assert_eq!(bot_detail(&bot(None)), t("workspace-settings-bot-running"));
    }
}

#[cfg(test)]
mod default_app_authority_tests {
    use super::*;
    use crate::authority::WorkspaceAuthority as _;
    use tilo_desktop_model::WorkspaceSlot;

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
            let mut ws = Workspace::new(WorkspaceSlot::from(7), "Docs");
            ws.set_owner("owner");

            assert!(
                ws.set_default_app_policy_as(
                    &"member".into(),
                    pdf.clone(),
                    desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_policy_as(
                &"owner".into(),
                pdf.clone(),
                desktop_policy::WorkspaceDefaultPolicy::App("so.tilo.viewer".into()),
                &resolver,
            )
            .unwrap();

            assert!(
                ws.set_default_app_choice_as(
                    &"owner".into(),
                    &"member".into(),
                    pdf.clone(),
                    desktop_policy::UserWorkspaceChoice::PersonalDefault,
                    &resolver
                )
                .is_err()
            );
            ws.set_default_app_choice_as(
                &"member".into(),
                &"member".into(),
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
