//! How the apps on a device take part in its workspaces: where a workspace stands for them, and
//! the membership commands their host carries into it.

/// A workspace's status once it changed ([`WorkspaceMemberships::workspace_status_change`]).
pub type WorkspaceStatusFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = runtime_collab::WorkspaceStatus> + Send>>;

/// Native workspace membership owner. Public callers choose only a closed typed
/// command; this host service supplies the private signing keys the host owns.
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

    /// Whether `workspace`'s membership actions are still to come here: this
    /// device holds the workspace, joins it or has it placed on its host, and its
    /// replica is not projected yet ([`Self::requires_workspace`] turns true
    /// once it is, which announces a policy change). An app opening in such a
    /// workspace waits for them; one opening in any other workspace
    /// unpublished is refused.
    fn membership_pending(&self, _workspace: &str) -> bool {
        false
    }

    /// Confirm that the canonical owner's durable store is still usable.
    fn health(&self) -> Result<(), String> {
        Err("workspace membership owner continuity is unavailable".into())
    }

    /// Write `command` into `workspace`'s governance as `caller`'s device.
    fn execute(
        &self,
        workspace: String,
        caller: tilo_user_identity::DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> WorkspaceMembershipFuture;

    /// The picture `person` (their root, bytes) set in their profile, as the image's own bytes:
    /// what a member row an app reads shows wherever it names them as an author. `None` before
    /// they set one, or where this host keeps no profile for them.
    fn person_photo(&self, person: &[u8; 32]) -> Option<Vec<u8>> {
        let _ = person;
        None
    }
}

/// A membership command on its way into the workspace it changes.
pub type WorkspaceMembershipFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
