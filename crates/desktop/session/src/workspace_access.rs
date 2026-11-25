//! The workspace's grants, as a view reads them: who and which apps may reach what, who gave each
//! of those, and the one action that takes a grant back.
//!
//! The reading is the one list every view of access draws: Files shows the grants that reach the
//! file or folder it has in front, and Settings shows every file and folder one app can reach.
//! Each entry says the app it is for, the workspace it is in and the host path of the file or
//! folder its node is, so a view matches it to the thing in front without a second list kept
//! anywhere. The files people chose for apps outside any workspace are entries of the same list
//! ([`tilo_dialogs::chosen`]), marked as the device's own.
//!
//! Revoking runs the actions that already take a grant back: a workspace grant is written into
//! the workspace's governance as the person who gave it (the same change the chooser writes when
//! it grants one), and a device-record choice is dropped from the device's record of chosen
//! files. Nothing here is a second place grants are kept.
//!
//! `known` says whether a workspace was read at all: a desktop that has not opened one yet says
//! it cannot check, never that no app was given anything.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_model::Workspace;
use tilo_dialogs::chosen::{AppKey, ChosenTable, Kind as ChosenKind};
use tilo_service::{Service, Started};
use tilo_session_client::daemon_documents::DaemonWritesFeed;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::desktop::workspace_settings::{
    AppDirectory, apply_revoke, listed_permissions, may_revoke,
};

use crate::i18n::t;
use crate::workspace_view::WorkspaceView;

/// The workspace's grants in the session's catalogue, as the Workspace package's root.
pub(crate) const RESOURCE: &str = "workspace/access";

/// The action that takes a grant or a device-record choice back, by its id.
pub(crate) const REVOKE: &str = "revoke";

/// The workspace's grants, as the session publishes them: its declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "workspace.access", label = "Access")]
pub struct AccessRoot;

/// The mount's record index moving: a file or folder renamed, moved or made, so the host path a
/// grant's node stands at is read again. The device's daemon tells every write it takes
/// ([`DaemonWritesFeed`]), and a write to a workspace's files tree is the tree moving.
#[derive(Clone, Default)]
pub(crate) struct MountMoves(Arc<AtomicU64>);

impl MountMoves {
    /// Follow `feed` on `runtime` from now on, `wake` called each time a workspace's files tree is
    /// written, so the reading is read again then.
    pub(crate) fn follow(
        feed: &DaemonWritesFeed,
        runtime: &tokio::runtime::Handle,
        wake: tilo_service::Wake,
    ) -> Self {
        let moves = Self::default();
        let counted = Arc::clone(&moves.0);
        let mut written = feed.follow();
        runtime.spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match written.recv().await {
                    Ok(write) => {
                        // A daemon that missed writes says so with an empty one: every path is
                        // read again, as the mounts may have moved with any of them.
                        if write.workspace.is_empty() || write.app == fs_protocol::tree::FILES_APP {
                            counted.fetch_add(1, Ordering::AcqRel);
                            wake();
                        }
                    }
                    // A write missed is in the tree: every path is read again.
                    Err(RecvError::Lagged(_)) => {
                        counted.fetch_add(1, Ordering::AcqRel);
                        wake();
                    }
                    Err(RecvError::Closed) => return,
                }
            }
        });
        moves
    }

    /// How often a workspace's files tree moved.
    fn revision(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

/// What the reading is made from besides the person's own request: the workspaces' governance as
/// the daemon published it, the device's record of chosen files, and the mounts' record index.
/// Each moves only when it changed, so the reading is read again when one of them did and never on
/// a clock.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Inputs {
    tree: u64,
    chosen: u64,
    mounts: u64,
}

/// The workspace's grants, published as `workspace/access`.
pub(crate) struct Served {
    handle: DesktopHandle,
    /// The person at this desktop: whose own grants read as "you", and who revokes as themselves.
    person: String,
    /// The person's workspaces as the device's daemon publishes them: where a grant taken back is
    /// written, through the same governance a chosen file's grant is written through.
    view: Option<Arc<WorkspaceView>>,
    /// The files people chose for apps outside any workspace: the device's own record.
    chosen: Option<Arc<ChosenTable>>,
    /// The mounts' record index, as the device's daemon tells each write it takes.
    mounts: MountMoves,
    /// What the last published reading was read from.
    read_from: RefCell<Option<Inputs>>,
    /// The reading last published.
    last: RefCell<Option<Value>>,
    /// The host path each record was last found at, and the mounts' revision it was read at.
    paths: RefCell<Paths>,
}

/// The host paths record ids were read as, at the mounts' revision they were read at.
#[derive(Default)]
struct Paths {
    at: Option<u64>,
    resolved: std::collections::HashMap<[u8; 16], Option<PathBuf>>,
}

impl Paths {
    /// The host path of `record` as the mounts at `mounts` say: read from the record index when
    /// the mounts moved since the ones held, or when `record` was not found before. A record the
    /// mounts do not hold yet — a mount that is not up — is no path kept, so it is looked for
    /// again the next time the reading is read.
    fn of(&mut self, mounts: u64, record: [u8; 16]) -> Option<&PathBuf> {
        if self.at != Some(mounts) {
            self.resolved.clear();
            self.at = Some(mounts);
        }
        if let std::collections::hash_map::Entry::Vacant(hole) = self.resolved.entry(record) {
            let found = tilo_app_launch::downloaded::workspace_resource(record)?;
            hole.insert(Some(found));
        }
        self.resolved.get(&record).and_then(Option::as_ref)
    }
}

impl Served {
    /// The grants of the workspaces `handle` holds, read for `person`.
    pub(crate) fn new(
        handle: DesktopHandle,
        person: String,
        view: Option<Arc<WorkspaceView>>,
        chosen: Option<Arc<ChosenTable>>,
        mounts: MountMoves,
    ) -> Self {
        Self {
            handle,
            person,
            view,
            chosen,
            mounts,
            read_from: RefCell::new(None),
            last: RefCell::new(None),
            paths: RefCell::new(Paths::default()),
        }
    }

    /// What the reading is made from now: the workspaces' governance, the device's chosen files
    /// and the mounts' record index.
    fn inputs(&self) -> Inputs {
        Inputs {
            tree: self.handle.revision(),
            chosen: self.chosen.as_ref().map_or(0, |table| table.revision()),
            mounts: self.mounts.revision(),
        }
    }

    /// The reading as the workspaces stand now.
    fn read_now(&self) -> Value {
        let person = &self.person;
        let chosen = self
            .chosen
            .as_ref()
            .map(|table| table.all())
            .unwrap_or_default();
        let mounts = self.mounts.revision();
        let mut paths = self.paths.borrow_mut();
        self.handle
            .read(|tree| reading_of(tree, person, &chosen, &mut paths, mounts))
    }

    /// The reading, read again when what it is made from moved since it was last read.
    fn current(&self) -> Value {
        if let (Some(read_from), Some(reading)) = (
            self.read_from.borrow().as_ref(),
            self.last.borrow().as_ref(),
        ) && *read_from == self.inputs()
        {
            return reading.clone();
        }
        self.reread()
    }

    /// Read the reading now, hold it, and answer it.
    fn reread(&self) -> Value {
        let inputs = self.inputs();
        let reading = self.read_now();
        *self.read_from.borrow_mut() = Some(inputs);
        *self.last.borrow_mut() = Some(reading.clone());
        reading
    }

    /// The action that takes the grant or the choice `id` back.
    fn revoke(&self, id: &str) -> Result<Started, ApiError> {
        let id = id.to_owned();
        if let Some((workspace, app, token)) = chosen_id(&id) {
            let table = self
                .chosen
                .as_ref()
                .ok_or_else(|| ApiError::Failed(t("session-access-no-records")))?;
            let key = AppKey::new(&workspace, &app)
                .ok_or_else(|| ApiError::Failed(t("session-access-no-records")))?;
            if !table.forget(&key, &token) {
                return Err(ApiError::Failed(t("session-access-already-gone")));
            }
            return Ok(Started::Done(output(&id)));
        }
        // A workspace's grant: the change is the workspace's, written through the governance it
        // admits like any other, as the person at this desktop.
        let Some(workspace) = self.find_grant(&id) else {
            return Err(ApiError::Failed(t("session-access-already-gone")));
        };
        let mut tree = self.handle.read(Clone::clone);
        let Some(held) = tree
            .workspaces
            .iter_mut()
            .find(|held| held.id().to_string() == workspace)
        else {
            return Err(ApiError::Failed(t("session-access-already-gone")));
        };
        // What the workspace's own policy says of the person taking it back: the same check the
        // sheet and the workspace's host make.
        let edit = apply_revoke(held, &self.person, &id)
            .map_err(|refusal| ApiError::Failed(refusal.to_string()))?;
        if edit.is_empty() {
            return Err(ApiError::Failed(t("session-access-already-gone")));
        }
        let Some(view) = self.view.clone() else {
            // A session that holds no workspace's governance has only the shell: the change
            // stands here, and nowhere else.
            self.handle.replace_tree(tree);
            return Ok(Started::Done(output(&id)));
        };
        let workspace_id = held.id();
        let written = crate::membership::Membership::of(&*view, workspace_id).changes(held, &edit);
        Ok(Started::Later(crate::landing::off_loop(async move {
            crate::membership::Membership::of(&*view, workspace_id)
                .write(&written)
                .await
                .map_err(ApiError::Failed)?;
            Ok(output(&id))
        })))
    }

    /// The workspace holding a grant with `id`, as the shell reads it.
    fn find_grant(&self, id: &str) -> Option<String> {
        self.handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|workspace| {
                    workspace
                        .policy
                        .grants
                        .iter()
                        .any(|grant| grant.id.as_str() == id)
                })
                .map(|workspace| workspace.id().to_string())
        })
    }
}

impl Service for Served {
    fn poll(&mut self) -> bool {
        let inputs = self.inputs();
        if self.read_from.borrow().as_ref() == Some(&inputs) {
            return false;
        }
        let reading = self.read_now();
        *self.read_from.borrow_mut() = Some(inputs);
        let changed = self.last.borrow().as_ref() != Some(&reading);
        *self.last.borrow_mut() = Some(reading);
        changed
    }

    /// The instant a grant this reading shows stops standing: a lifetime that ends, or the
    /// person's own restriction, which says whether they may take one back.
    fn wakes_in(&self) -> Option<Duration> {
        let now = tilo_clock::unix_now();
        let context = tilo_action_policy::Context::at_secs(now);
        let person = &self.person;
        self.handle.read(|tree| {
            let mut soonest: Option<u64> = None;
            let mut consider = |at: u64| {
                if at > now.saturating_mul(1000) && soonest.is_none_or(|held| at < held) {
                    soonest = Some(at);
                }
            };
            for workspace in &tree.workspaces {
                if let Some(at) = workspace.policy.restricted_until(person, &context) {
                    consider(at);
                }
                for grant in &workspace.policy.grants {
                    if let tilo_action_policy::Lifetime::Until(at) = grant.lifetime {
                        consider(at);
                    }
                }
                for capability in &workspace.sandbox {
                    if let tilo_action_policy::Lifetime::Until(at) = capability.lifetime {
                        consider(at);
                    }
                }
            }
            soonest.map(|at| Duration::from_millis(at.saturating_sub(now.saturating_mul(1000))))
        })
    }

    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            tilo_resource::LiveRow::typed::<AccessRoot>(RESOURCE, t("session-workspace-access"))
                .into_resource()
                .with_action(tilo_actions::ActionSpec::typed(
                    REVOKE,
                    t("session-access-revoke"),
                    "",
                    tilo_actions::ExecutionLocus::ClientLocal,
                    vec![tilo_actions::ActionParam::required(
                        "id",
                        tilo_actions::ParamType::Text,
                        "The grant or chosen file to take back.",
                    )],
                    |_| {
                        tilo_actions::EncodedAction::encode_operation("workspace.access", &())
                            .unwrap_or_else(|error| tilo_actions::EncodedAction {
                                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
                                payload: error.to_string().into_bytes(),
                            })
                    },
                )),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.current())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        if resource != RESOURCE || action != REVOKE {
            return Err(ApiError::Unknown(ActionAddress::new(resource, action)));
        }
        let id = arguments
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ApiError::InvalidArguments("an id names what to take back".to_owned())
            })?;
        self.revoke(id)
    }
}

/// What a revoke answers.
fn output(id: &str) -> tilo_actions::ActionOutput {
    tilo_actions::ActionOutput {
        value: Some(serde_json::json!({ "revoked": id })),
        resources: Vec::new(),
    }
}

/// The workspace, app and token a device-record choice's id names (`chosen:<workspace>:<app>:<token>`).
fn chosen_id(id: &str) -> Option<(String, String, String)> {
    let rest = id.strip_prefix("chosen:")?;
    let (workspace, rest) = rest.split_once(':')?;
    let (app, token) = rest.split_once(':')?;
    (!workspace.is_empty() && !app.is_empty() && !token.is_empty())
        .then(|| (workspace.to_owned(), app.to_owned(), token.to_owned()))
}

/// The grants the workspaces of `tree` give, as the Permissions tab lists them, each with the app
/// it is for and the file or folder its node is; and the files chosen outside any workspace, in
/// the same list.
fn reading_of(
    tree: &tilo_desktop_model::Layout,
    person: &str,
    chosen: &[(AppKey, String, tilo_dialogs::chosen::Chosen)],
    paths: &mut Paths,
    mounts: u64,
) -> Value {
    let apps = crate::workspace_settings::Directory::default();
    let now = tilo_clock::unix_now();
    let mut groups: Vec<(String, String, Vec<Value>)> = Vec::new();
    for workspace in &tree.workspaces {
        let grants: Vec<Value> = listed_permissions(workspace, &apps, person)
            .into_iter()
            .filter_map(|grant| entry_of(workspace, &apps, person, &grant, now, paths, mounts))
            .collect();
        groups.push((workspace.id().to_string(), workspace.name.clone(), grants));
    }
    // The files people chose outside any workspace: the device's own record, listed in the
    // workspace it was chosen in.
    for (key, token, held) in chosen {
        let entry = chosen_entry(key, token, held);
        match groups.iter_mut().find(|(id, _, _)| id == key.workspace()) {
            Some((_, _, grants)) => grants.push(entry),
            None => groups.push((key.workspace().to_owned(), String::new(), vec![entry])),
        }
    }
    let workspaces: Vec<Value> = groups
        .into_iter()
        .filter(|(_, _, grants)| !grants.is_empty())
        .map(|(id, name, grants)| {
            serde_json::json!({
                "id": id,
                "name": name,
                "grants": grants,
            })
        })
        .collect();
    serde_json::json!({
        "known": !tree.workspaces.is_empty(),
        "workspaces": workspaces,
    })
}

/// One grant the workspace lists, with what a view needs to show and take it back; `None` for a
/// grant that is no file or folder access (an app's network reach, a device capability), which the
/// Permissions tab lists and this view does not.
fn entry_of(
    workspace: &Workspace,
    apps: &dyn AppDirectory,
    person: &str,
    listed: &tilo_session_core::desktop::workspace_settings::ListedPermission,
    now: u64,
    paths: &mut Paths,
    mounts: u64,
) -> Option<Value> {
    let held = workspace
        .policy
        .grants
        .iter()
        .find(|held| held.id.as_str() == listed.id)?;
    let node = &held.node;
    let path = match (
        node.id_at(tilo_action_policy::Level::App),
        node.id_at(tilo_action_policy::Level::Resource),
    ) {
        (Some(app), Some(record)) if app == fs_protocol::tree::FILES_APP => {
            let uuid = uuid::Uuid::parse_str(record).ok()?;
            paths
                .of(mounts, *uuid.as_bytes())
                .map(|path| path.to_string_lossy().into_owned())?
        }
        _ => return None,
    };
    let (granted_by_name, granted_by_kind, mine) =
        tilo_session_core::desktop::workspace_settings::granted_by_parts(
            workspace,
            &held.granted_by,
            person,
            apps,
        );
    Some(serde_json::json!({
        "id": listed.id,
        "section": listed.section,
        "holder": listed.holder,
        "allows": listed.allows,
        "expiry": listed.expiry,
        "granted_by": listed.granted_by,
        "app": held.grantee.app_id().unwrap_or_default(),
        "path": path,
        "workspace": workspace.id().to_string(),
        "holder_kind": holder_kind(&held.grantee),
        "granted_by_name": granted_by_name,
        "granted_by_kind": granted_by_kind,
        "granted_by_you": mine,
        "device": false,
        "can_revoke": may_revoke(workspace, person, held, now),
    }))
}

/// What kind of holder a grant names, as a view shows it.
fn holder_kind(grantee: &tilo_action_policy::Grantee) -> &'static str {
    use tilo_action_policy::Grantee;
    match grantee {
        Grantee::Member(tilo_action_policy::MemberId::Agent { .. }) => "agent",
        Grantee::Member(_) => "person",
        Grantee::App(_) => "app",
        Grantee::Role(_) | Grantee::CustomRole(_) | Grantee::AnyKey => "role",
    }
}

/// One device-record choice, as an entry of the same list: a file or folder an app was given
/// outside any workspace, which the person who chose it may take back.
fn chosen_entry(key: &AppKey, token: &str, held: &tilo_dialogs::chosen::Chosen) -> Value {
    let name = ui::local_identity::load()
        .map(|identity| identity.display_name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| t("session-access-you"));
    let allows = match held.kind {
        ChosenKind::Read => t("session-access-allows-read"),
        ChosenKind::Save => t("session-access-allows-write"),
        ChosenKind::Folder { write: false } => t("session-access-allows-folder"),
        ChosenKind::Folder { write: true } => t("session-access-allows-folder-write"),
    };
    serde_json::json!({
        "id": format!("chosen:{}:{}:{}", key.workspace(), key.app(), token),
        "section": tilo_session_core::desktop::workspace_settings::RecipientKind::App.section_label(),
        "holder": crate::app_names::claimant(key.app()),
        "allows": allows,
        "expiry": ui::format::lifetime(tilo_action_policy::Lifetime::UntilRevoked),
        "granted_by": name,
        "app": key.app(),
        "path": held.host.to_string_lossy(),
        "workspace": key.workspace(),
        "holder_kind": "app",
        "granted_by_name": name,
        "granted_by_kind": "you",
        "granted_by_you": true,
        "device": true,
        "can_revoke": true,
    })
}

#[cfg(test)]
mod tests {
    use collab_types::caps::{Access, CapOverride, DeviceCap, Effect, Scope};
    use tilo_action_policy::Grantee;
    use tilo_desktop_model::{Layout, Workspace, WorkspaceId};

    use super::*;

    fn workspace_with_caps(sandbox: Vec<CapOverride>) -> Layout {
        let mut workspace = Workspace::new(WorkspaceId::from_u128(7), "settings-access-test");
        workspace.sandbox = sandbox;
        let mut tree = Layout::default();
        tree.workspaces.push(workspace);
        tree
    }

    fn paths() -> Paths {
        Paths::default()
    }

    /// What an app was given beyond its documents is said with what it allows, on which
    /// workspace, in the words the workspace's own Permissions tab uses.
    #[test]
    fn an_apps_grant_reads_on_the_workspace_that_gave_it() {
        let tree = workspace_with_caps(vec![
            CapOverride::grant(
                Grantee::person("ada"),
                Scope::FsPath("/photos".into()),
                Access::Write,
            )
            .through("so.tilo.photos"),
            CapOverride::grant(
                Grantee::person("ada"),
                Scope::Device(DeviceCap::DesktopControl),
                Access::Write,
            )
            .through("so.tilo.terminal"),
            // A restriction is not something an app was given.
            CapOverride::new(
                Grantee::person("ada"),
                Scope::NetHost("example.com".into()),
                Effect::Restrict,
                Access::Read,
            )
            .through("so.tilo.browser"),
        ]);
        let read = reading_of(&tree, "ada", &[], &mut paths(), 0);
        assert_eq!(read["known"], serde_json::json!(true));
        // The capabilities are no file access, so this view holds none of them; the Permissions
        // tab still lists them.
        assert_eq!(read["workspaces"], serde_json::json!([]));
    }

    /// A workspace that gave nothing is said so, and a desktop that has opened none says it
    /// cannot check rather than that there are none.
    #[test]
    fn nothing_given_and_nothing_read_are_said_apart() {
        let empty = reading_of(
            &workspace_with_caps(Vec::new()),
            "ada",
            &[],
            &mut paths(),
            0,
        );
        assert_eq!(empty["known"], serde_json::json!(true));
        assert_eq!(empty["workspaces"], serde_json::json!([]));
        let unread = reading_of(&Layout::default(), "ada", &[], &mut paths(), 0);
        assert_eq!(unread["known"], serde_json::json!(false));
    }

    /// A device-record choice is listed in the workspace it was chosen in, with the app that holds
    /// it and the path it names.
    #[test]
    fn a_device_record_choice_is_the_apps_file_outside_a_workspace() {
        let mut tree = workspace_with_caps(Vec::new());
        tree.workspaces[0].name = "Ada's".to_owned();
        let workspace = tree.workspaces[0].id().to_string();
        let key = AppKey::new(&workspace, "so.tilo.notes").expect("a key");
        let held = tilo_dialogs::chosen::Chosen {
            reference: PathBuf::from("/run/tilo/chosen/records/t/plan.md"),
            name: "plan.md".to_owned(),
            host: PathBuf::from("/home/ada/plan.md"),
            kind: ChosenKind::Read,
            save_sibling: None,
        };
        let read = reading_of(
            &tree,
            "ada",
            &[(key, "token".to_owned(), held)],
            &mut paths(),
            0,
        );
        assert_eq!(read["known"], serde_json::json!(true));
        let grants = read["workspaces"][0]["grants"].as_array().expect("grants");
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0]["device"], serde_json::json!(true));
        assert_eq!(grants[0]["path"], serde_json::json!("/home/ada/plan.md"));
        assert_eq!(grants[0]["app"], serde_json::json!("so.tilo.notes"));
    }

    /// The session publishes the reading on its root and on no other.
    #[test]
    fn the_service_publishes_the_reading_where_a_view_reads_it() {
        let handle = DesktopHandle::new(workspace_with_caps(Vec::new()), 1);
        let served = Served::new(handle, "ada".into(), None, None, MountMoves::default());
        let published = served.resources();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].id.as_str(), RESOURCE);
        assert!(
            published[0].actions.iter().any(|spec| spec.id == REVOKE),
            "the view's one action is published"
        );
        let read = served.read(RESOURCE).expect("the reading");
        assert_eq!(read["known"], serde_json::json!(true));
        assert!(served.read("workspace/content").is_none());
    }

    /// A chosen id names its workspace, app and token, and nothing else does.
    #[test]
    fn a_chosen_id_parses_to_what_takes_it_back() {
        assert_eq!(
            chosen_id("chosen:7:so.tilo.notes:abc"),
            Some(("7".to_owned(), "so.tilo.notes".to_owned(), "abc".to_owned()))
        );
        assert_eq!(chosen_id("chosen:app-id"), None);
        assert_eq!(chosen_id("some-grant"), None);
    }

    /// A workspace that lands on the shell reaches the reading on the session's next pass, with no
    /// clock between: what the reading is made from is what the shell holds.
    #[test]
    fn a_workspace_that_lands_reaches_the_reading_without_a_clock() {
        let handle = DesktopHandle::new(Layout::default(), 1);
        let mut served = Served::new(
            handle.clone(),
            "ada".into(),
            None,
            None,
            MountMoves::default(),
        );
        assert!(served.poll(), "the first reading is published");
        assert_eq!(
            served.read(RESOURCE).expect("the reading")["known"],
            serde_json::json!(false)
        );
        assert!(!served.poll(), "nothing moved is no change");
        // The workspace's governance lands on the shell, as the daemon's reading projects it.
        handle.replace_tree(workspace_with_caps(Vec::new()));
        assert!(served.poll(), "a workspace that landed is news");
        assert_eq!(
            served.read(RESOURCE).expect("the reading")["known"],
            serde_json::json!(true)
        );
        assert!(!served.poll(), "nothing moved since is no change");
    }

    /// A file chosen for an app outside any workspace reaches the reading as it is chosen, and
    /// leaves it as it is taken back: the device's record is what the reading follows.
    #[test]
    fn a_choice_made_reaches_the_reading_without_a_clock() {
        let tree = workspace_with_caps(Vec::new());
        let workspace = tree.workspaces[0].id().to_string();
        let table = Arc::new(ChosenTable::new());
        let mut served = Served::new(
            DesktopHandle::new(tree, 1),
            "ada".into(),
            None,
            Some(Arc::clone(&table)),
            MountMoves::default(),
        );
        assert!(served.poll(), "the first reading is published");
        assert_eq!(
            served.read(RESOURCE).expect("the reading")["workspaces"],
            serde_json::json!([])
        );
        assert!(!served.poll(), "nothing moved is no change");

        let dir = tempfile::tempdir().expect("a folder");
        let file = dir.path().join("plan.md");
        std::fs::write(&file, b"plan").expect("a file");
        let key = AppKey::new(&workspace, "so.tilo.notes").expect("a key");
        table
            .grant(&key, &file, ChosenKind::Read)
            .expect("the person chose it");
        assert!(served.poll(), "a choice made is news");
        let read = served.read(RESOURCE).expect("the reading");
        assert_eq!(read["workspaces"][0]["grants"][0]["app"], "so.tilo.notes");

        let token = table.tokens(&key)[0].clone();
        assert!(table.forget(&key, &token), "the choice is taken back");
        assert!(served.poll(), "a choice taken back is news");
        assert_eq!(
            served.read(RESOURCE).expect("the reading")["workspaces"],
            serde_json::json!([])
        );
    }
}
