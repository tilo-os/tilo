//! The workspace's connected accounts as this device's daemon publishes them to the apps
//! attached to it: the mail, calendar and cloud-storage accounts each workspace keeps, each with
//! the kind of service it is, the address it signs in at, its provider and how its last sync
//! went, read as `accounts`.
//!
//! The reading is the workspace service's, which this daemon holds ([`Workspaces`]): the same
//! accounts record [`list`](tilo_workspace_service::workspace_accounts::LIST) answers and its members'
//! windows change, and no secret in it — a secret stays in the workspace's vault. A watch follows
//! every write to that record, here or on another device, so a window listing the accounts draws
//! again as one is added, signed in again or removed anywhere.

use std::sync::Arc;

use tilo_app_host::services::Caller;

use super::reading::{Changes, ReadLater, Served};

pub use tilo_workspace_service::workspace_accounts::RESOURCE as ACCOUNTS;

/// The workspace's connected accounts, published to the apps this device serves, read where the
/// workspaces keep them.
#[must_use]
pub fn published(workspaces: &Arc<crate::device::workspaces::ServiceWorkspaces>) -> Served {
    let reading = Arc::clone(workspaces);
    let read: Arc<ReadLater> = Arc::new(move |caller: Caller| {
        let reading = Arc::clone(&reading);
        Box::pin(async move { reading.copies().accounts_reading(&caller.workspace).await })
    });
    let (moved, changes) = tokio::sync::watch::channel(0_u64);
    let watching = Arc::clone(workspaces);
    // On the process's runtime: the daemon publishes this before any task of its own runs.
    tilo_rt::app().spawn(async move {
        let mut writes = watching.copies().document_writes();
        loop {
            match writes.recv().await {
                Ok(write) if accounts_record(&write) => {
                    moved.send_modify(|moved| *moved = moved.wrapping_add(1));
                }
                Ok(_) => {}
                // A follower that fell behind missed writes of every record: read the accounts
                // again rather than leave the window showing an older set.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    moved.send_modify(|moved| *moved = moved.wrapping_add(1));
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    let changes: Arc<Changes> = Arc::new(move || changes.clone());
    Served::later(ACCOUNTS, read, changes)
}

/// Whether `write` is a write to a workspace's accounts record.
fn accounts_record(write: &tilo_workspace_service::workspaces::ResourceWrite) -> bool {
    write.app == tilo_app_id::apps::SETTINGS
        && write.record == tilo_workspace_service::workspaces::account_logins::accounts_document()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The daemon publishes the accounts reading before any of its own tasks run: building it
    /// where no Tokio runtime is entered must not panic, which is what the in-process daemon
    /// does.
    #[test]
    fn publishing_the_accounts_reading_needs_no_entered_runtime() {
        let runtime = tokio::runtime::Runtime::new().expect("a runtime to hold a workspace");
        let (workspaces, _dir, _key, _cert, _paths) = runtime.block_on(async {
            crate::device::workspaces::ServiceWorkspaces::test_hosting_person(
                tilo_server_protocol::WorkspaceId::from_u128(1),
            )
            .await
        });
        // Outside the runtime: publishing spawns the watcher on the process's own runtime.
        let served = published(&workspaces);
        drop(served);
        drop(runtime);
    }
}
