//! The person's authority in a workspace, as an app is handed it.
//!
//! The launcher admits a workspace app as a viewer unless the session says
//! otherwise: what it says is a projection of the workspace's policy, narrowed
//! to the grants that can decide one of this app's actions
//! ([`AppActionAuthorityProjection`]), which the launcher binds read-only into
//! the app and the app decides every action against. So a launch publishes it
//! here, before the process starts, and holds it for as long as the person's
//! session does: the app reads a projection as current only while this session
//! holds it (`tilo_files::native::held`), so a session that ends withdraws
//! every app's authority with it and no clock has to renew anything.
//!
//! One projection per workspace and app, under the per-user runtime directory
//! and named by a hash of what it is for: the launcher binds only that
//! directory into the sandbox, so an app reaches its own authority and no other
//! host path. A workspace whose policy this session does not hold publishes
//! none, and its apps take the launcher's participation — the read-only one.
//!
//! Republishing follows the policy: [`Authority::refresh`] replaces the
//! projections of a workspace whose policy changed, and the app hears the
//! replacement and judges its next action by it, so a role someone is given or
//! taken back reaches the app they already run.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io;
use std::path::PathBuf;

use collab_types::action_authority::{AppActionAuthorityProjection, MAX_PROJECTION_BYTES};
use tilo_files::native::held::Held;
use tilo_files::native::{atomic, held};

use crate::workspace::Workspace;

/// What a session has published, and its hold on each projection.
#[derive(Default)]
pub struct Authority {
    published: RefCell<BTreeMap<(String, String), Published>>,
}

/// One published projection and this session's hold on its file.
struct Published {
    projection: AppActionAuthorityProjection,
    /// The file is current only while this lives.
    _held: Held,
}

impl Authority {
    /// Nothing published yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether anything is published at all: nothing to refresh when not.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.published.borrow().is_empty()
    }

    /// The workspaces anything is published in, in a settled order.
    #[must_use]
    pub fn workspaces(&self) -> Vec<String> {
        let mut rooms: Vec<String> = self
            .published
            .borrow()
            .keys()
            .map(|(workspace, _)| workspace.clone())
            .collect();
        rooms.sort();
        rooms.dedup();
        rooms
    }

    /// Publish what `workspace`'s policy lets the person at this device do in
    /// `app`, and return the file to bind into the launch (`--action-authority`);
    /// `None` when the launch names no workspace or the session holds no policy
    /// for it, and then the app takes the launcher's participation.
    ///
    /// The projection already held for that app is kept as it is while the
    /// policy behind it has not changed, so a launch costs no write.
    ///
    /// # Errors
    /// The projection cannot be written, or is one the launcher would refuse.
    pub fn publish(&self, workspace: &Workspace, app: &str) -> io::Result<Option<PathBuf>> {
        let Some(policy) = workspace
            .policy
            .as_ref()
            .filter(|_| workspace.is_workspace())
        else {
            return Ok(None);
        };
        if policy.workspace != workspace.id {
            // A policy of another workspace decides nothing for this one: the
            // app is admitted as the launcher would admit it alone.
            return Ok(None);
        }
        let projection =
            AppActionAuthorityProjection::project(policy, app, &workspace.person, false);
        projection
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let key = (workspace.id.clone(), app.to_owned());
        if self
            .published
            .borrow()
            .get(&key)
            .is_some_and(|published| published.projection == projection)
        {
            return Ok(Some(path(workspace, app)));
        }
        let bytes = serde_json::to_vec(&projection).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("serialising {}'s authority: {error}", workspace.person),
            )
        })?;
        if bytes.len() > MAX_PROJECTION_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{app}'s authority in {} exceeds its byte bound",
                    workspace.id
                ),
            ));
        }
        let path = path(workspace, app);
        if let Some(directory) = path.parent() {
            atomic::create_private_directory(directory)?;
        }
        // Created 0600 and held from before the name is in place, so the app
        // that reads it finds a projection this session vouches for. Neither
        // the file nor its directory is synced: this is the runtime directory,
        // which does not outlive the session, and this runs on the session's
        // loop.
        let held = held::publish(&path, &bytes, 0o600)?;
        self.published.borrow_mut().insert(
            key,
            Published {
                projection,
                _held: held,
            },
        );
        Ok(Some(path))
    }

    /// Publish again what is published in `workspace`, from its policy as the
    /// desk holds it now: what the person's authority there became. A
    /// projection whose policy did not change is not written.
    ///
    /// # Errors
    /// A projection could not be written; the ones after it are left as they
    /// were.
    pub fn refresh(&self, workspace: &Workspace) -> io::Result<()> {
        let apps: Vec<String> = self
            .published
            .borrow()
            .keys()
            .filter(|(room, _)| room == &workspace.id)
            .map(|(_, app)| app.clone())
            .collect();
        for app in apps {
            self.publish(workspace, &app)?;
        }
        Ok(())
    }
}

/// Where an app's projection of `workspace` is published.
fn path(workspace: &Workspace, app: &str) -> PathBuf {
    let named = format!("{}\0{}\0{}", workspace.id, app, workspace.person);
    let digest = blake3::hash(named.as_bytes());
    directory(workspace)
        .join(digest.to_hex().as_str())
        .join("current.json")
}

/// The directory the projections are published under: beside the grants, in
/// the runtime directory the session keeps its launch files in.
fn directory(workspace: &Workspace) -> PathBuf {
    workspace
        .directory
        .parent()
        .unwrap_or(&workspace.directory)
        .join(ACTION_AUTHORITY_DIRECTORY)
}

/// The name the launcher's projections live under, beside the capabilities.
const ACTION_AUTHORITY_DIRECTORY: &str = "action-authority";

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tilo_action_policy::{ActionKind, Answer, Context, Role, WorkspacePolicy};

    const APP: &str = "so.tilo.notes";
    const OWNER: &str = "person-a";
    const MEMBER: &str = "person-b";

    /// A workspace whose governance is `policy`, launched by `person`.
    fn workspace(directory: &Path, policy: WorkspacePolicy, person: &str) -> Workspace {
        let mut workspace = Workspace::under(directory, policy.workspace.clone());
        workspace.policy = Some(policy);
        workspace.person = person.to_owned();
        workspace
    }

    /// Whether the app's own new-note action is admitted by what `file` holds:
    /// what the notes bar asks (`ActionNeed::of(Undoable)` on its own node).
    fn may_add_notes(file: &Path) -> bool {
        let bytes = held::read(file, MAX_PROJECTION_BYTES)
            .expect("readable")
            .expect("held by the session");
        let projection: AppActionAuthorityProjection =
            serde_json::from_slice(&bytes).expect("a projection");
        projection.validate().expect("a valid projection");
        projection
            .authority()
            .decide(
                &projection.app_node().action("new-note"),
                ActionKind::Undoable,
                &Context::at(0),
            )
            .answer
            == Answer::Allow
    }

    #[test]
    fn a_launch_hands_its_app_the_owners_authority_in_a_workspace_nobody_else_joined() {
        let directory = tempfile::tempdir().expect("a directory");
        let authority = Authority::new();
        let room = workspace(
            directory.path(),
            WorkspacePolicy::new("workspace-7", OWNER),
            OWNER,
        );
        let file = authority
            .publish(&room, APP)
            .expect("published")
            .expect("a projection to bind");
        assert_eq!(
            file.file_name().and_then(|name| name.to_str()),
            Some("current.json")
        );
        assert!(
            file.starts_with(directory.path().join("tilo").join("action-authority")),
            "beside the grants in the session's runtime directory: {}",
            file.display()
        );
        let read: AppActionAuthorityProjection = serde_json::from_slice(
            &held::read(&file, MAX_PROJECTION_BYTES)
                .expect("readable")
                .expect("held"),
        )
        .expect("a projection");
        assert_eq!(
            (
                read.workspace.as_str(),
                read.app_id.as_str(),
                read.person.as_str()
            ),
            ("workspace-7", APP, OWNER)
        );
        assert!(
            read.policy
                .grants
                .iter()
                .all(|grant| grant.node.overlaps(&read.app_node())),
            "only the grants that can decide this app's actions reach it"
        );
        assert!(
            may_add_notes(&file),
            "the person who made the workspace may make a note in it"
        );
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(&file)
                .expect("the file")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o600, "the person's authority is owner-only");
        let parent = file.parent().expect("a directory");
        let mode = {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::metadata(parent)
                .expect("the directory")
                .permissions()
                .mode()
        };
        assert_eq!(mode & 0o777, 0o700, "as the launcher requires of the bind");
    }

    #[test]
    fn a_member_takes_the_part_the_workspace_gives_them_and_a_viewer_is_offered_nothing() {
        let directory = tempfile::tempdir().expect("a directory");
        let mut policy = WorkspacePolicy::new("workspace-7", OWNER);
        policy.members.insert(MEMBER.into(), Role::Editor);
        let editor = workspace(directory.path(), policy.clone(), MEMBER);
        let authority = Authority::new();
        let file = authority
            .publish(&editor, APP)
            .expect("published")
            .expect("a projection");
        assert!(may_add_notes(&file), "an editor may make a note");

        policy.members.insert(MEMBER.into(), Role::Viewer);
        let viewer = workspace(directory.path(), policy, MEMBER);
        let authority = Authority::new();
        let file = authority
            .publish(&viewer, APP)
            .expect("published")
            .expect("a projection");
        assert!(
            !may_add_notes(&file),
            "someone who may only look is offered no New note"
        );
    }

    #[test]
    fn a_policy_that_changed_is_published_over_and_one_that_did_not_is_kept() {
        let directory = tempfile::tempdir().expect("a directory");
        let authority = Authority::new();
        let mut policy = WorkspacePolicy::new("workspace-7", OWNER);
        policy.members.insert(MEMBER.into(), Role::Editor);
        let mut launched = workspace(directory.path(), policy.clone(), MEMBER);
        let file = authority
            .publish(&launched, APP)
            .expect("published")
            .expect("a projection");
        assert!(may_add_notes(&file));
        let kept = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(&file).expect("the file").ino()
        };
        // Nothing changed: the projection is not written again.
        assert_eq!(
            authority.publish(&launched, APP).expect("published"),
            Some(file.clone())
        );
        let inode = {
            use std::os::unix::fs::MetadataExt as _;
            std::fs::metadata(&file).expect("the file").ino()
        };
        assert_eq!(inode, kept, "an unchanged policy costs no write");

        // The workspace takes the member's part back: the app already running
        // there hears the new authority.
        policy.members.insert(MEMBER.into(), Role::Viewer);
        launched.policy = Some(policy);
        assert_eq!(authority.workspaces(), vec!["workspace-7".to_owned()]);
        authority.refresh(&launched).expect("refreshed");
        assert!(!may_add_notes(&file), "the app no longer may make a note");
    }

    #[test]
    fn a_launch_outside_a_workspace_or_without_its_policy_binds_none() {
        let directory = tempfile::tempdir().expect("a directory");
        let authority = Authority::new();
        assert!(authority.is_empty());
        let outside = Workspace::under(directory.path(), String::new());
        assert_eq!(authority.publish(&outside, APP).expect("nothing"), None);
        let held = Workspace::under(directory.path(), "workspace-7");
        assert_eq!(
            authority.publish(&held, APP).expect("nothing"),
            None,
            "a workspace whose policy this session does not hold gets the launcher's participation"
        );
        let mut foreign = held;
        foreign.policy = Some(WorkspacePolicy::new("workspace-8", OWNER));
        assert_eq!(
            authority.publish(&foreign, APP).expect("nothing"),
            None,
            "a policy of another workspace decides nothing here"
        );
        assert!(authority.is_empty());
    }
}
