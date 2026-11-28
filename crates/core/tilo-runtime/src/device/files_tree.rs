//! The trees of the workspaces this daemon holds, served on a socket to the files daemon that
//! mounts them ([`tilo_fs::tree_sync::remote`]).
//!
//! The daemon is the device's one workspace service, so the mount syncs through it: a
//! connection is taken from a process of this account alone, the keys of a tree's group come
//! from the daemon's own identity service, the bytes a mount lacks are asked of the daemon's
//! record content, and the bytes it keeps are the ones that record content hands the
//! workspace. The access list the mount enforces is the workspaces' own: each one's policy
//! and members, and the folders it grants its apps, as its governance keeps them. It is made
//! again on every change of what a decision reads, and at the moment a member's time-limited
//! grant lapses.
//!
//! Where each mounted folder stands, as its mount says it, is this device's resource
//! [`FOLDERS_APP`]: the person's shell follows it to show what reached the workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tilo_workspace_service::files::socket::{self, Daemons, Keys, Offers, Serving};
use tilo_workspace_service::files::{FOLDERS_APP, Folders};
use tilo_workspace_service::service::ServiceSlot;
use tokio::sync::{Notify, watch};

use super::workspaces::ServiceWorkspaces;

/// The trees' socket while it is served.
pub(crate) struct FilesTree {
    _serving: Serving,
    access: tokio::task::JoinHandle<()>,
}

impl Drop for FilesTree {
    fn drop(&mut self) {
        self.access.abort();
    }
}

/// Serve the trees of the workspaces `workspaces` holds at `path`, for the mounts of `person`
/// on `device`, with their keys from the identity service at `identity`; each mount joins
/// `daemons`, through which the daemon's record content reads and keeps its files' bytes.
/// `None` before the daemon's broker serves record content: a mount would have no bytes to read.
pub(crate) fn serve(
    path: &Path,
    workspaces: &Arc<ServiceWorkspaces>,
    daemons: Arc<Daemons>,
    identity: PathBuf,
    (person, device): (String, [u8; 32]),
) -> Option<FilesTree> {
    let content = workspaces.record_content_service()?;
    let service = ServiceSlot::default();
    service.install(Arc::clone(workspaces.service()));
    let keys: Keys = Arc::new(move |workspace| {
        let keys = tilo_fs::hydration::FileKeys::new(workspace, identity.clone());
        Box::pin(async move {
            let keys = keys.access().await.map_err(|error| format!("{error:#}"))?;
            keys.to_bytes()
                .map(|bytes| bytes.to_vec())
                .map_err(|error| format!("{error:#}"))
        })
    });
    let (told, access) = watch::channel(None);
    let copies = workspaces.copies().clone();
    let follows = tilo_rt::app().spawn(follow_access(copies, (person.clone(), device), told));
    // The workspace this device's session shows, as the session keeps it in the device's own
    // record: a mount serves that one's files, and reads it here before it opens a tree.
    let (told_shown, shown) = watch::channel(None);
    {
        let workspaces = Arc::clone(workspaces);
        let own = tilo_action_policy::roles::workspace_of_person(&person);
        let nil = tilo_desktop_model::WorkspaceId::nil().to_string();
        tilo_rt::app().spawn(async move {
            loop {
                let now = workspaces
                    .device_records()
                    .and_then(|records| records.get(tilo_session_client::session_shown::SHOWN))
                    .and_then(|value| {
                        tilo_session_client::session_shown::shown_workspace(&value)
                            .map(str::to_owned)
                    })
                    .filter(|workspace| own.as_deref() != Some(workspace.as_str()))
                    .filter(|workspace| workspace != &nil)
                    // A workspace the device does not hold yet is one a mount cannot open: it
                    // is named once the device has it, and the mount opens then.
                    .filter(|workspace| {
                        workspaces
                            .copies()
                            .held_workspaces()
                            .iter()
                            .any(|held| held == workspace)
                    });
                told_shown.send_if_modified(|was| {
                    if *was == now {
                        return false;
                    }
                    *was = now;
                    true
                });
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });
    }
    // Where each folder a mount holds stands: how much it has to send and to fetch.
    let folders = Folders::default();
    workspaces.copies().publish_own(
        FOLDERS_APP,
        tilo_workspace_service::workspaces::OwnReaders::ThisDevice,
        Arc::new(folders.clone()),
    );
    let serving = socket::serve(
        path,
        service,
        content,
        // Every process of this account is admitted, as the socket's own check of its peer
        // decides: the mount is the account's, as the daemon is.
        Arc::new(|_, _| true),
        Offers {
            daemons,
            keys,
            access,
            shown,
        },
        folders,
    );
    Some(FilesTree {
        _serving: serving,
        access: follows,
    })
}

/// Tell the mounts the workspaces' access list each time it changes, as long as they listen.
async fn follow_access(
    copies: tilo_workspace_service::workspaces::Workspaces,
    (person, device): (String, [u8; 32]),
    told: watch::Sender<Option<String>>,
) {
    let changed = Arc::new(Notify::new());
    let watch = tilo_identity::policy_change::Watch::new({
        let changed = Arc::clone(&changed);
        move || changed.notify_one()
    });
    loop {
        let seen = watch.arm();
        let listed = copies.files_access(&person, Some(device)).await;
        if let Some(at) = listed.refresh_at {
            tilo_identity::policy_change::wake_at(at.saturating_mul(1_000_000));
        }
        match serde_json::to_string(&listed.authority) {
            Ok(json) => {
                told.send_if_modified(|was| {
                    let new = was.as_deref() != Some(json.as_str());
                    if new {
                        *was = Some(json);
                    }
                    new
                });
            }
            Err(error) => tracing::warn!(%error, "the workspaces' file access list is not JSON"),
        }
        if told.is_closed() {
            return;
        }
        if tilo_identity::policy_change::epoch() == seen {
            changed.notified().await;
        }
    }
}
