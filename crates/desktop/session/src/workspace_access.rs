//! The workspace's grants, as Settings reads them: what its apps, its people and their agents
//! were given beyond their own documents, and how long each of those stands.
//!
//! It is the same list the workspace's own Permissions tab shows ([`listed_permissions`]), read
//! here for the person at this desktop so Settings can summarise it beside this device's own
//! answers without being a second place grants are kept or changed. The reading is a root of the
//! Workspace package (`workspace/access`), published by the session for the workspaces this
//! desktop shows.
//!
//! `known` says whether a workspace was read at all: a desktop that has not opened one yet says
//! it cannot check, never that no app was given anything.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ApiError, ResourceSpec};
use tilo_service::{Service, Started};
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::desktop::workspace_settings::listed_permissions;

use crate::i18n::t;

/// The workspace's grants in the session's catalogue, as the Workspace package's root.
pub(crate) const RESOURCE: &str = "workspace/access";

/// The workspace's grants, as the session publishes them: its declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "workspace.access", label = "Access")]
pub struct AccessRoot;

/// How often the workspaces are read again: a grant changed here or on another device reaches
/// the reading without every pass of the loop looking through each one's grants.
const REFRESH: Duration = Duration::from_secs(1);

/// The workspace's grants, published as `workspace/access`.
pub(crate) struct Served {
    handle: DesktopHandle,
    /// The person at this desktop: what their own grants read as ("You").
    person: String,
    /// The reading last published, and when it was read.
    last: RefCell<Option<(Instant, Value)>>,
}

impl Served {
    /// The grants of the workspaces `handle` holds, read for `person`.
    pub(crate) fn new(handle: DesktopHandle, person: String) -> Self {
        Self {
            handle,
            person,
            last: RefCell::new(None),
        }
    }

    /// The reading as the workspaces stand now.
    fn read_now(&self) -> Value {
        let person = &self.person;
        self.handle.read(|tree| reading_of(tree, person))
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
                .into_resource(),
        ]
    }

    fn read(&self, resource: &str) -> Option<Value> {
        (resource == RESOURCE).then(|| self.current())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &Map<String, Value>,
    ) -> Result<Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
    }
}

/// The grants the workspaces of `tree` give, as the Permissions tab lists them: each workspace
/// that gives any, by the name the device showing it calls it.
pub(crate) fn reading_of(tree: &tilo_desktop_model::Layout, person: &str) -> Value {
    let apps = crate::workspace_settings::Directory::default();
    let mut workspaces: Vec<Value> = Vec::new();
    for workspace in &tree.workspaces {
        let grants: Vec<Value> = listed_permissions(workspace, &apps, person)
            .into_iter()
            .map(|grant| {
                serde_json::json!({
                    "id": grant.id,
                    "section": grant.section,
                    "holder": grant.holder,
                    "allows": grant.allows,
                    "expiry": grant.expiry,
                    "granted_by": grant.granted_by.unwrap_or_default(),
                })
            })
            .collect();
        if grants.is_empty() {
            continue;
        }
        workspaces.push(serde_json::json!({
            "name": workspace.name,
            "grants": grants,
        }));
    }
    serde_json::json!({
        "known": !tree.workspaces.is_empty(),
        "workspaces": workspaces,
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
        let read = reading_of(&tree, "ada");
        assert_eq!(read["known"], serde_json::json!(true));
        let workspaces = read["workspaces"].as_array().expect("workspaces");
        assert_eq!(workspaces.len(), 1);
        let grants = workspaces[0]["grants"].as_array().expect("grants");
        assert_eq!(grants.len(), 2, "the restriction is no grant: {read}");
        assert!(
            grants.iter().any(|grant| grant["allows"]
                .as_str()
                .is_some_and(|allows| allows.contains("photos"))),
            "the path it reaches is said: {read}"
        );
        assert!(
            grants
                .iter()
                .any(|grant| grant["section"] == serde_json::json!("Apps")),
            "an app's grant is listed under the apps: {read}"
        );
    }

    /// A workspace that gave nothing is said so, and a desktop that has opened none says it
    /// cannot check rather than that there are none.
    #[test]
    fn nothing_given_and_nothing_read_are_said_apart() {
        let empty = reading_of(&workspace_with_caps(Vec::new()), "ada");
        assert_eq!(empty["known"], serde_json::json!(true));
        assert_eq!(empty["workspaces"], serde_json::json!([]));
        let unread = reading_of(&Layout::default(), "ada");
        assert_eq!(unread["known"], serde_json::json!(false));
    }

    /// The session publishes the reading on its root and on no other.
    #[test]
    fn the_service_publishes_the_reading_where_a_view_reads_it() {
        let handle = DesktopHandle::new(workspace_with_caps(Vec::new()), 1);
        let served = Served::new(handle, "ada".into());
        let published = served.resources();
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].id.as_str(), RESOURCE);
        let read = served.read(RESOURCE).expect("the reading");
        assert_eq!(read["known"], serde_json::json!(true));
        assert!(served.read("workspace/content").is_none());
    }
}
