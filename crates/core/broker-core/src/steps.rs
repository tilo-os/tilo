//! What a window's app reports about resuming where it was, carried to the session that runs
//! the window.

/// What a window's app reports about resuming where it was, in the workspace it runs in. The
/// session that runs the window keeps it.
pub trait ContinuationServices: Send + Sync {
    /// Reports one application instance's continuation metadata.
    fn report(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: String,
        instance: u64,
        schema: u16,
        metadata: Vec<u8>,
    ) -> Result<(), String>;
    /// Clears one application instance's continuation metadata.
    fn clear(
        &self,
        workspace: tilo_desktop_model::WorkspaceId,
        app: &str,
        instance: u64,
    ) -> Result<(), String>;
}
