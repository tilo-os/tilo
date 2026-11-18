//! The app surfaces this host lacks, asked of its workspaces until they
//! arrive.
//!
//! A member's change names the content hash of the surface it was written
//! with, and the host decides it against that version's surface: the one it
//! runs the app with, or one the workspace keeps for it
//! ([`crate::surfaces`]). What it lacks it asks the workspace's members for
//! ([`Workspaces::ask_surface`]) — the member that wrote the change answers
//! with the bytes, or any other member that has them — and asks again while it
//! is missing and somebody may have joined since, until a member answers and
//! the changes held for it are decided.
//!
//! Nothing here reads a registry: a surface is content of the workspace, and
//! the workspace is where it comes from.

use std::sync::Arc;

use tilo_lock::MutexRecover as _;

use super::Workspaces;
use crate::surfaces::address;

/// How long before the host asks a workspace again for a surface that did not
/// come: past it, another member may have joined with it.
const ASK_AGAIN: std::time::Duration = std::time::Duration::from_secs(60);

impl Workspaces {
    /// Note that this host lacks the surface `hash` of `app` in `workspace`,
    /// so a change naming it is decided once it is asked for and answered
    /// ([`Self::follow_missing_surfaces`]).
    pub(super) fn note_missing_surface(&self, workspace: &str, app: &str, hash: &str) {
        let Some(hash) = address(hash) else {
            return;
        };
        self.inner
            .content
            .missing
            .lock_recover()
            .insert(hash, (workspace.to_owned(), app.to_owned()));
    }

    /// Ask each workspace for the surfaces this host lacks, until they arrive
    /// or the workspaces are no longer governed here.
    pub(super) fn follow_missing_surfaces(&self) {
        let workspaces = Arc::downgrade(&self.inner);
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        runtime.spawn(async move {
            let mut again = tokio::time::interval(ASK_AGAIN);
            again.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let Some(inner) = workspaces.upgrade() else {
                    return;
                };
                Self { inner }.ask_missing_surfaces().await;
                again.tick().await;
            }
        });
    }

    /// Ask for every surface this host lacks and has not been answered with
    /// yet, and forget those that arrived.
    async fn ask_missing_surfaces(&self) {
        let missing: Vec<([u8; 32], String, String)> = self
            .inner
            .content
            .missing
            .lock_recover()
            .iter()
            .map(|(hash, (workspace, app))| (*hash, workspace.clone(), app.clone()))
            .collect();
        for (hash, workspace, app) in missing {
            let name = hex::encode(hash);
            if crate::surfaces::holds(&name) {
                self.inner.content.missing.lock_recover().remove(&hash);
                continue;
            }
            self.ask_surface(&workspace, &app, &name).await;
        }
    }
}
