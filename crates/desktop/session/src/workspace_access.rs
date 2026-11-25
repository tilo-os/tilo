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
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_desktop_model::{Layout, Workspace};
use tilo_dialogs::chosen::{AppKey, ChosenTable, Kind as ChosenKind};
use tilo_service::{Service, Started};
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

/// How often the workspaces are read again: a grant changed here or on another device reaches the
/// reading without every pass of the loop looking through each one's grants.
const REFRESH: Duration = Duration::from_secs(1);

/// How long a grant's host path is trusted before the mount's record index is read again: a file
/// that moved shows where it is, and a view drawn between two reads is a second old at worst.
const PATHS_FOR: Duration = Duration::from_secs(5);

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
    /// The reading last published, and when it was read.
    last: RefCell<Option<(Instant, Value)>>,
    /// The host path each record was last found at, and when.
    paths: RefCell<Paths>,
}

/// The host paths record ids were read as, and when that reading is trusted until.
#[derive(Default)]
struct Paths {
    read_at: Option<Instant>,
    resolved: std::collections::HashMap<[u8; 16], Option<PathBuf>>,
}

impl Paths {
    /// The host path of `record` now, read from the mounts when the ones held are older than
    /// [`PATHS_FOR`].
    fn of(&mut self, record: [u8; 16]) -> Option<&PathBuf> {
        let now = Instant::now();
        if self
            .read_at
            .is_none_or(|at| now.duration_since(at) >= PATHS_FOR)
        {
            self.resolved.clear();
            self.read_at = Some(now);
        }
        self.resolved
            .entry(record)
            .or_insert_with(|| tilo_app_launch::downloaded::workspace_resource(record))
            .as_ref()
    }
}

impl Served {
    /// The grants of the workspaces `handle` holds, read for `person`.
    pub(crate) fn new(
        handle: DesktopHandle,
        person: String,
        view: Option<Arc<WorkspaceView>>,
        chosen: Option<Arc<ChosenTable>>,
    ) -> Self {
        Self {
            handle,
            person,
            view,
            chosen,
            last: RefCell::new(None),
            paths: RefCell::new(Paths::default()),
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
        let mut paths = self.paths.borrow_mut();
        self.handle
            .read(|tree| reading_of(tree, person, &chosen, &mut paths))
    }

    /// The reading, read now when the one held is older than [`REFRESH`].
    fn current(&self) -> Value {
        let now = Instant::now();
        if let Some((at, reading)) = self.last.borrow().as_ref()
            && now.duration_since(*at) < REFRESH
        {
            return reading.clone();
        }
        let reading = self.read_now();
        *self.last.borrow_mut() = Some((now, reading.clone()));
        reading
    }

    /// The action that takes the grant or the choice `id` back.
    fn revoke(&mut self, id: &str) -> Result<Started, ApiError> {
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
        let now = Instant::now();
        if let Some((at, _)) = self.last.borrow().as_ref()
            && now.duration_since(*at) < REFRESH
        {
            return false;
        }
        let reading = self.read_now();
        let changed = self
            .last
            .borrow()
            .as_ref()
            .is_none_or(|(_, was)| was != &reading);
        *self.last.borrow_mut() = Some((now, reading));
        changed
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
pub(crate) fn reading_of(
    tree: &tilo_desktop_model::Layout,
    person: &str,
    chosen: &[(AppKey, String, tilo_dialogs::chosen::Chosen)],
    paths: &mut Paths,
) -> Value {
    let apps = crate::workspace_settings::Directory::default();
    let now = tilo_clock::unix_now();
    let mut groups: Vec<(String, String, Vec<Value>)> = Vec::new();
    for workspace in &tree.workspaces {
        let grants: Vec<Value> = listed_permissions(workspace, &apps, person)
            .into_iter()
            .filter_map(|grant| entry_of(workspace, &apps, person, &grant, now, paths))
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
                .of(*uuid.as_bytes())
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
        Grantee::Member(tilo_action_policy::MemberId::Agent(_)) => "agent",
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
        let read = reading_of(&tree, "ada", &[], &mut paths());
        assert_eq!(read["known"], serde_json::json!(true));
        // The capabilities are no file access, so this view holds none of them; the Permissions
        // tab still lists them.
        assert_eq!(read["workspaces"], serde_json::json!([]));
    }

    /// A workspace that gave nothing is said so, and a desktop that has opened none says it
    /// cannot check rather than that there are none.
    #[test]
    fn nothing_given_and_nothing_read_are_said_apart() {
        let empty = reading_of(&workspace_with_caps(Vec::new()), "ada", &[], &mut paths());
        assert_eq!(empty["known"], serde_json::json!(true));
        assert_eq!(empty["workspaces"], serde_json::json!([]));
        let unread = reading_of(&Layout::default(), "ada", &[], &mut paths());
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
        let served = Served::new(handle, "ada".into(), None, None);
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
}
