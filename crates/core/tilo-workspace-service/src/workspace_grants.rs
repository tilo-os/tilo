//! What a workspace shares out of itself: the grants that give a part of it — a folder, a
//! document, an item of one — to someone besides the person sharing it, and the folder
//! capabilities a person holds outside the workspace's tree.
//!
//! The entries are read from the workspace's own records every time: its policy's grants and the
//! sandbox capabilities it keeps, never a list kept beside them. Membership is not here — being a
//! member of the workspace is its people's to show ([`crate::workspaces::resources`]) — so "Bob
//! holds the brief" cannot collapse into "Bob is a member".
//!
//! Who may read the list is the workspace's to say: a person sees what it shares out only when
//! they may grant or take back a grant in it, so a member who only reads the workspace is not
//! told what its owner shared with whom. The rows name people as the workspace names them, never
//! by their key.

use collab_types::caps::{Access, Effect, Scope};
use tilo_action_policy::{
    Answer, Context, Grant, Grantee, Labels, Level, TreePath, WorkspacePolicy, roles,
};
use tilo_action_types::ActionKind;
use tilo_desktop_model::Workspace;
use tilo_member_types::MemberId;

/// One part of a workspace shared with someone, as the overview's list shows it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SharedOut {
    /// The workspace it is shared out of.
    pub workspace: String,
    /// What revoking it names: the grant's id, or the capability's.
    pub id: String,
    /// The resource's address in the workspace, empty for a folder a capability reaches.
    pub node: String,
    /// The app the shared part belongs to, by its id; empty for a folder a capability reaches.
    pub app: String,
    /// The host path a capability reaches, empty for a grant on a resource.
    pub path: String,
    /// What is shared, as a person reads it: the app and kind it belongs to, then the id the
    /// resource or item is kept under. A view showing it on a device names a file or folder by
    /// where its mount has it.
    pub what: String,
    /// Whom it is shared with, by the name the workspace knows them by; empty when nobody named
    /// them.
    pub holder: String,
    /// The holder's id: their person, agent or bot id, or empty for anyone with the link.
    pub holder_id: String,
    /// What the holder is: `member`, `outside`, `agent`, `bot` or `link`.
    pub holder_kind: &'static str,
    /// What it lets them do: `read` or `write`.
    pub access: &'static str,
    /// Who gave it, by the name the workspace knows them by; empty when the giver was no person.
    pub granted_by: String,
    /// Whether the person reading this gave it.
    pub granted_by_you: bool,
    /// When it was given, in Unix milliseconds, as the workspace's membership log records it; 0
    /// where the log does not know.
    pub granted_at: u64,
    /// Whether the person reading this may take it back here.
    pub can_revoke: bool,
}

/// Whether `person` may see what `policy`'s workspace shares out: whoever may grant or take back a
/// grant in it, and nobody else.
#[must_use]
pub fn sees_grants(policy: &WorkspacePolicy, person: &str, now_ms: u64) -> bool {
    let now = now_ms / 1_000;
    policy.may_change_at(person, roles::governance::GRANT, now)
        || policy.may_change_at(person, roles::governance::REVOKE, now)
}

/// What `workspace` shares out, as `person` may see it at `now_ms` (Unix milliseconds): one row
/// per grant or capability, named as the workspace names the people holding it. Empty for someone
/// the workspace does not let administer its grants ([`sees_grants`]).
#[must_use]
pub fn shared_out(workspace: &Workspace, person: &str, now_ms: u64) -> Vec<SharedOut> {
    let policy = &workspace.policy;
    if !sees_grants(policy, person, now_ms) {
        return Vec::new();
    }
    let now = now_ms / 1_000;
    let cx = Context::at(now_ms);
    let may_revoke = may_revoke(policy, person, now);
    let root = policy.node();
    let me = names(workspace);
    let mut out: Vec<SharedOut> = Vec::new();
    for grant in &policy.grants {
        if let Some(row) = grant_row(grant, workspace, person, &cx, &root, may_revoke, &me) {
            out.push(row);
        }
    }
    for cap in &workspace.sandbox {
        if let Some(row) = capability_row(cap, workspace, person, now_ms, may_revoke, &me) {
            out.push(row);
        }
    }
    out.sort_by(|a, b| {
        a.what
            .cmp(&b.what)
            .then_with(|| a.holder.cmp(&b.holder))
            .then_with(|| a.id.cmp(&b.id))
    });
    out
}

/// Whether `person` may take a grant back in `policy` now.
fn may_revoke(policy: &WorkspacePolicy, person: &str, now: u64) -> bool {
    !policy.is_blocked(person)
        && policy
            .restricted_until(person, &Context::at_secs(now))
            .is_none()
        && policy.may_change_at(person, roles::governance::REVOKE, now)
}

/// The names `workspace` knows its people by, for the rows.
struct Names<'a>(&'a Workspace);

fn names(workspace: &Workspace) -> Names<'_> {
    Names(workspace)
}

impl Names<'_> {
    /// The name `who` goes by here, or empty for someone nobody named.
    fn of(&self, who: &Grantee) -> String {
        who.person_id()
            .and_then(|person| self.0.person_name(person))
            .unwrap_or_default()
            .to_owned()
    }

    /// The name the member `who` goes by here, or empty.
    fn member(&self, who: &MemberId) -> String {
        self.0
            .members
            .iter()
            .find(|member| member.id == *who)
            .map(|member| member.name.trim().to_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_default()
    }
}

/// One grant of `workspace`'s policy as a row, `None` for a grant that is no part of it shared
/// out: the workspace whole, a role or administrator's grant, an app's own reach, a refusal, one
/// that has lapsed, or the reader's own.
fn grant_row(
    grant: &Grant,
    workspace: &Workspace,
    person: &str,
    cx: &Context<'_>,
    root: &TreePath,
    may_revoke: bool,
    names: &Names<'_>,
) -> Option<SharedOut> {
    if grant.node == *root
        || !grant.node.is_within(root)
        || grant.answer != Answer::Allow
        || !grant.is_live(cx)
        || roles::administrator_named(&grant.id).is_some()
        || roles::is_app_records_grant(grant)
    {
        return None;
    }
    let holder = match &grant.grantee {
        Grantee::Member(member) => member,
        // Anyone holding the link: a document or folder opened to any key.
        Grantee::AnyKey => {
            return Some(SharedOut {
                workspace: workspace.id().to_string(),
                id: grant.id.to_string(),
                node: grant.node.to_string(),
                app: grant.node.id_at(Level::App).unwrap_or_default().to_owned(),
                path: String::new(),
                what: what_of(&grant.node),
                holder: String::new(),
                holder_id: String::new(),
                holder_kind: "link",
                access: access_of(grant.kinds.contains(ActionKind::Undoable)),
                granted_by: names.of(&grant.granted_by),
                granted_by_you: grant.granted_by.person_id() == Some(person),
                granted_at: granted_at(workspace, grant),
                can_revoke: may_revoke,
            });
        }
        Grantee::Role(_) | Grantee::CustomRole(_) | Grantee::App(_) => return None,
    };
    let holder_person = holder.person_id();
    if holder_person == Some(person) {
        return None;
    }
    let (holder_name, holder_kind) = match holder {
        MemberId::Person(_) => {
            let name = names.of(&grant.grantee);
            let member = holder_person.is_some_and(|person| {
                workspace.policy.is_owner(person) || workspace.policy.members.contains_key(person)
            });
            (name, if member { "member" } else { "outside" })
        }
        MemberId::Agent { .. } => (names.member(holder), "agent"),
        MemberId::Bot(_) => (names.member(holder), "bot"),
    };
    Some(SharedOut {
        workspace: workspace.id().to_string(),
        id: grant.id.to_string(),
        node: grant.node.to_string(),
        app: grant.node.id_at(Level::App).unwrap_or_default().to_owned(),
        path: String::new(),
        what: what_of(&grant.node),
        holder: holder_name,
        holder_id: holder.to_string(),
        holder_kind,
        access: access_of(grant.kinds.contains(ActionKind::Undoable)),
        granted_by: names.of(&grant.granted_by),
        granted_by_you: grant.granted_by.person_id() == Some(person),
        granted_at: granted_at(workspace, grant),
        can_revoke: may_revoke,
    })
}

/// When `grant` was given as `workspace`'s membership log records it, in Unix milliseconds; 0
/// where the log does not know.
fn granted_at(workspace: &Workspace, grant: &Grant) -> u64 {
    workspace
        .grant_times
        .get(grant.id.as_str())
        .copied()
        .unwrap_or(0)
}

/// One capability a person holds in `workspace` as a row, `None` for a refusal, one that has
/// lapsed, one no person holds, or the reader's own.
fn capability_row(
    cap: &collab_types::caps::CapOverride,
    workspace: &Workspace,
    person: &str,
    now_ms: u64,
    may_revoke: bool,
    names: &Names<'_>,
) -> Option<SharedOut> {
    if cap.effect != Effect::Grant || !cap.is_live_at(Some(now_ms)) {
        return None;
    }
    // A part of the workspace is a file or a folder: what reaches a device or a host is the
    // app's own reach, which its page lists ([`crate::workspaces::actions`]).
    if !matches!(cap.scope, Scope::FsPath(_) | Scope::FsResource(_)) {
        return None;
    }
    let holder = cap.grantee.person_id()?;
    if holder == person {
        return None;
    }
    let holder_name = names.of(&cap.grantee);
    let member = workspace.policy.is_owner(holder) || workspace.policy.members.contains_key(holder);
    let (what, path) = scope_words(&cap.scope);
    let giver = match &cap.granted_by {
        Some(member) => {
            let id = member.person_id().unwrap_or_default();
            if id.is_empty() {
                names.member(member)
            } else {
                workspace.person_name(id).unwrap_or_default().to_owned()
            }
        }
        None => String::new(),
    };
    Some(SharedOut {
        workspace: workspace.id().to_string(),
        id: cap.id.clone(),
        node: String::new(),
        app: String::new(),
        path,
        what,
        holder: holder_name,
        holder_id: holder.to_owned(),
        holder_kind: if member { "member" } else { "outside" },
        access: access_of(cap.access == Access::Write),
        granted_by: giver,
        granted_by_you: cap
            .granted_by
            .as_ref()
            .is_some_and(|member| member.person_id() == Some(person)),
        granted_at: 0,
        can_revoke: may_revoke,
    })
}

/// What a capability's scope reaches, as a person reads it, and the host path it stands at.
fn scope_words(scope: &Scope) -> (String, String) {
    match scope {
        Scope::FsPath(path) => {
            let name = path
                .trim_end_matches('/')
                .rsplit('/')
                .find(|part| !part.is_empty())
                .unwrap_or(path)
                .to_owned();
            (name, path.clone())
        }
        Scope::FsResource(record) => (record.clone(), record.clone()),
        // Not a part of the workspace: an app's reach, which its own page lists.
        Scope::Device(_) | Scope::NetHost(_) | Scope::NetCalls => (String::new(), String::new()),
    }
}

/// Whether a grant or capability lets the holder `write`.
const fn access_of(write: bool) -> &'static str {
    if write { "write" } else { "read" }
}

/// Where a grant applies, named the way people know the workspace's parts: the app by its name, a
/// resource or item by the id it is stored under, then the rest of the path.
fn what_of(node: &TreePath) -> String {
    tilo_action_policy::describe_node(&node.within_workspace(), &Named)
}

/// The app and kind names a grant's path reads through.
struct Named;

impl Labels for Named {
    fn label(&self, level: Level, id: &str) -> Option<String> {
        match level {
            Level::Workspace | Level::Resource | Level::Item => None,
            Level::App | Level::Kind | Level::Action | Level::Field => Some(
                tilo_actions::field_type::label_of(id.rsplit('.').next().unwrap_or(id)),
            ),
        }
    }

    fn everything(&self) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    //! What a workspace shares out, read from the workspace's own grants and capabilities: a
    //! folder and a document given to two people, the roles and app grants that are not shares,
    //! and the member who may not administer the workspace's grants seeing none.

    use super::*;
    use collab_types::caps::{Access, CapOverride, Effect, Scope};
    use tilo_action_policy::{Change, Role};
    use tilo_action_types::KindSet;
    use tilo_desktop_model::WorkspaceId;

    const OWNER: &str = "owner0000000000000000000000000000000000000000000000000000000000000";
    const ADA: &str = "ada00000000000000000000000000000000000000000000000000000000000000";
    const BO: &str = "bo0000000000000000000000000000000000000000000000000000000000000000";
    const VIEWER: &str = "vie000000000000000000000000000000000000000000000000000000000000000";
    /// Well after the epoch, well before any grant here ends.
    const NOW_MS: u64 = 1_800_000_000_000;

    /// A workspace `OWNER` owns, with `ADA` and `BO` as members it named.
    fn workspace() -> Workspace {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(7), "Studio");
        workspace.policy = WorkspacePolicy::new("studio", OWNER);
        workspace.policy.members.insert(ADA.into(), Role::Editor);
        workspace.policy.members.insert(BO.into(), Role::Editor);
        workspace.named_people([(ADA, "Ada"), (BO, "Bo")]);
        workspace
    }

    /// The node a document of an app stands at in the workspace.
    fn document(app: &str, kind: &str, id: &str) -> TreePath {
        TreePath::workspace("studio")
            .app(app)
            .kind(kind)
            .resource(id)
    }

    /// `OWNER` shares `node` with `holder` as `kinds` allow.
    fn share(workspace: &mut Workspace, id: &str, holder: Grantee, node: TreePath, kinds: KindSet) {
        let grant = Grant::new(
            id,
            holder,
            node,
            kinds,
            Answer::Allow,
            Grantee::person(OWNER),
        );
        workspace
            .policy
            .change_all_as_at(OWNER, vec![Change::Grant(grant)], NOW_MS / 1_000)
            .expect("the owner may share");
    }

    #[test]
    fn a_folder_and_a_document_are_listed_with_who_holds_them_and_what_they_may_do() {
        let mut workspace = workspace();
        workspace.sandbox.push(
            CapOverride::grant(
                Grantee::person(ADA),
                Scope::FsPath("/var/workspace/brief".into()),
                Access::Read,
            )
            .with_id("cap-brief")
            .granted_by(MemberId::person(OWNER)),
        );
        share(
            &mut workspace,
            "bo-plan",
            Grantee::person(BO),
            document("so.tilo.notes", "notes.notebook", "plan"),
            KindSet::READ_UNDOABLE,
        );
        workspace
            .grant_times
            .insert("bo-plan".into(), 1_700_000_000_000);

        let shares = shared_out(&workspace, OWNER, NOW_MS);
        assert_eq!(shares.len(), 2, "{shares:?}");
        let folder = shares
            .iter()
            .find(|share| share.id == "cap-brief")
            .expect("the folder is shared");
        assert_eq!(folder.what, "brief");
        assert_eq!(folder.path, "/var/workspace/brief");
        assert_eq!(folder.holder, "Ada");
        assert_eq!(folder.holder_kind, "member");
        assert_eq!(folder.access, "read");
        assert!(folder.can_revoke);
        assert!(folder.granted_by_you, "the owner shared it");

        let plan = shares
            .iter()
            .find(|share| share.id == "bo-plan")
            .expect("the document is shared");
        assert_eq!(plan.what, "Notes › Notebook › plan");
        assert_eq!(
            plan.node,
            "workspace:studio/app:so.tilo.notes/kind:notes.notebook/resource:plan"
        );
        assert_eq!(plan.holder, "Bo");
        assert_eq!(plan.access, "write");
        assert_eq!(
            plan.granted_at, 1_700_000_000_000,
            "when the log says it was given"
        );
    }

    #[test]
    fn a_member_who_may_not_administer_the_grants_is_told_nothing() {
        let mut workspace = workspace();
        workspace.policy.members.insert(VIEWER.into(), Role::Viewer);
        share(
            &mut workspace,
            "bo-plan",
            Grantee::person(BO),
            document("so.tilo.notes", "notes.notebook", "plan"),
            KindSet::READ,
        );
        assert!(
            shared_out(&workspace, VIEWER, NOW_MS).is_empty(),
            "a viewer may not see what the workspace shares out"
        );
        assert!(!shared_out(&workspace, OWNER, NOW_MS).is_empty());
    }

    #[test]
    fn membership_roles_and_an_apps_own_reach_are_not_parts_shared_out() {
        let mut workspace = workspace();
        let root = workspace.policy.node();
        // A grant on the workspace itself is membership, not a part of it shared.
        share(
            &mut workspace,
            "ada-whole",
            Grantee::person(ADA),
            root,
            KindSet::READ,
        );
        // An administrator's grants are its role, which its people show.
        let admin =
            tilo_action_policy::roles::administrator_grant("studio", ADA, Grantee::person(OWNER));
        workspace
            .policy
            .change_all_as_at(OWNER, vec![Change::Grant(admin)], NOW_MS / 1_000)
            .expect("the owner may grant");
        // An app's own records are what installing it gave it.
        let records = tilo_action_policy::roles::app_records_grant("studio", "so.tilo.mail", OWNER);
        share(
            &mut workspace,
            records.id.as_str(),
            records.grantee.clone(),
            records.node.clone(),
            KindSet::ALL,
        );
        // A role holds the workspace's parts by its preset, not as a share.
        share(
            &mut workspace,
            "everyone-plan",
            Grantee::role(Role::Viewer),
            document("so.tilo.notes", "notes.notebook", "plan"),
            KindSet::READ,
        );
        // What the reader holds is not shared with anyone else.
        share(
            &mut workspace,
            "owner-plan",
            Grantee::person(OWNER),
            document("so.tilo.notes", "notes.notebook", "mine"),
            KindSet::READ,
        );
        assert!(
            shared_out(&workspace, OWNER, NOW_MS).is_empty(),
            "{:?}",
            shared_out(&workspace, OWNER, NOW_MS)
        );
    }

    #[test]
    fn a_lapsed_share_is_not_shared() {
        let mut workspace = workspace();
        let grant = Grant::new(
            "ada-for-a-while",
            Grantee::person(ADA),
            document("so.tilo.notes", "notes.notebook", "plan"),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(OWNER),
        )
        .with_lifetime(tilo_action_types::Lifetime::Until(1_000_000));
        workspace
            .policy
            .change_all_as_at(OWNER, vec![Change::Grant(grant)], NOW_MS / 1_000)
            .expect("the owner may share");
        workspace.sandbox.push(
            CapOverride::grant(
                Grantee::person(ADA),
                Scope::FsPath("/var/workspace/brief".into()),
                Access::Read,
            )
            .with_id("cap-lapsed")
            .lasting(tilo_action_types::Lifetime::Until(1_000_000)),
        );
        assert!(shared_out(&workspace, OWNER, NOW_MS).is_empty());
    }

    #[test]
    fn someone_outside_the_workspace_and_a_link_are_listed_as_what_they_are() {
        let mut workspace = workspace();
        share(
            &mut workspace,
            "cleo-post",
            Grantee::person("cleo"),
            document("so.tilo.notes", "notes.notebook", "plan"),
            KindSet::READ,
        );
        // Anyone with the link: the workspace's entry writes such a grant, and nobody grants one
        // by hand, so the fixture keeps it as the entry would.
        workspace.policy.grants.push(Grant::new(
            "anyone-post",
            Grantee::AnyKey,
            document("so.tilo.notes", "notes.notebook", "poster"),
            KindSet::READ,
            Answer::Allow,
            Grantee::person(OWNER),
        ));
        let shares = shared_out(&workspace, OWNER, NOW_MS);
        let outside = shares
            .iter()
            .find(|share| share.id == "cleo-post")
            .expect("the person outside holds it");
        assert_eq!(outside.holder_kind, "outside");
        assert_eq!(outside.holder, "", "nobody here named them");
        let link = shares
            .iter()
            .find(|share| share.id == "anyone-post")
            .expect("anyone with the link holds it");
        assert_eq!(link.holder_kind, "link");
        assert_eq!(link.access, "read");
    }

    #[test]
    fn a_capability_that_restricts_is_not_a_share() {
        let mut workspace = workspace();
        workspace.sandbox.push(
            CapOverride::new(
                Grantee::person(ADA),
                Scope::NetHost("example.com".into()),
                Effect::Restrict,
                Access::Read,
            )
            .with_id("cap-restrict"),
        );
        assert!(shared_out(&workspace, OWNER, NOW_MS).is_empty());
    }
}
