//! How the apps on a device take part in its workspaces: where a workspace stands for them, and
//! who is in them.

/// A workspace's status once it changed ([`WorkspaceMemberships::workspace_status_change`]).
pub type WorkspaceStatusFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = runtime_collab::WorkspaceStatus> + Send>>;

/// The workspaces this device holds, as the apps on it read them.
pub trait WorkspaceMemberships: Send + Sync {
    fn requires_workspace(&self, workspace: &str) -> bool;

    /// Where `workspace` stands for the apps on this device: ready, waiting for
    /// its join or its placement on its host, or refused, as when no host
    /// holds it and it has no documents to open. A join is held while it
    /// waits and refused with the reason, in place of a failure further in.
    fn workspace_status(&self, _workspace: &str) -> runtime_collab::WorkspaceStatus {
        runtime_collab::WorkspaceStatus::Ready
    }

    /// `workspace`'s status once it is no longer `seen`, woken by the change that
    /// ends it rather than by asking again.
    fn workspace_status_change(
        &self,
        workspace: &str,
        seen: runtime_collab::WorkspaceStatus,
    ) -> WorkspaceStatusFuture {
        let now = self.workspace_status(workspace);
        if now == seen {
            Box::pin(std::future::pending())
        } else {
            Box::pin(std::future::ready(now))
        }
    }

    /// Confirm that the canonical owner's durable store is still usable.
    fn health(&self) -> Result<(), String> {
        Err("workspace membership owner continuity is unavailable".into())
    }

    /// The picture `person` (their root, bytes) set in their profile, as the image's own bytes:
    /// what a member row an app reads shows wherever it names them as an author. `None` before
    /// they set one, or where this host keeps no profile for them.
    fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        let _ = person;
        None
    }
}
