//! What a host answers for this device's writes in the workspaces it keeps: where a document
//! write is admitted and made durable, and the receipt an effect leaves.

use collab_types::SignedOp;
use std::future::Future;
use std::pin::Pin;
use tilo_desktop_model::WorkspaceId;

/// A document write on its way to the record it changes: the write as the record admitted it.
pub type DocumentFuture = Pin<Box<dyn Future<Output = Result<SignedOp, String>> + Send>>;
/// An effect receipt on its way into the workspace it was served in.
pub type EffectFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// What a host answers for this device's writes.
pub trait WorkspaceCommits: collab_types::signer::MaybeSendSync + 'static {
    /// Whether `workspace`'s writes and effects are this host's to record.
    fn projects_workspace(&self, workspace: WorkspaceId) -> bool;
    /// Whether the record this host keeps is usable now.
    fn health(&self) -> Result<(), String>;
    /// Admit `envelope`, this device's signed write of `app`'s `document` in `workspace`, and
    /// make it durable. Answers the write as the record took it.
    fn document(
        &self,
        workspace: String,
        app: String,
        document: String,
        readers: Option<String>,
        envelope: SignedOp,
        resource_type: Option<String>,
    ) -> DocumentFuture;
    /// Record the terminal receipt an effect an app ran left, `redacted`, in `workspace`.
    fn effect(&self, workspace: String, redacted: Vec<u8>) -> EffectFuture;
}
