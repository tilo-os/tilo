//! What a host answers for this device's writes in the workspaces it keeps: the receipt an
//! effect leaves.

use std::future::Future;
use std::pin::Pin;
use tilo_desktop_model::WorkspaceId;

/// An effect receipt on its way into the workspace it was served in.
pub type EffectFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// What a host answers for this device's writes.
pub trait WorkspaceCommits: collab_types::signer::MaybeSendSync + 'static {
    /// Whether `workspace`'s writes and effects are this host's to record.
    fn projects_workspace(&self, workspace: WorkspaceId) -> bool;
    /// Whether the record this host keeps is usable now.
    fn health(&self) -> Result<(), String>;
    /// Record the terminal receipt an effect an app ran left, `redacted`, in `workspace`.
    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture;
}
