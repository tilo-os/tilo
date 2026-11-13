//! The content a participant keeps for its workspaces' records, answered to a member that asks for
//! it ([`neighbour_wire::content`]): a page's pictures, an agent's content store.

/// What this device keeps of the content its workspaces' records reference.
/// `Send` on every host: a page's session carries it in the commands its handle sends.
pub trait Kept: Send + Sync {
    /// The bytes kept under `hash` (hex) that a record of `app` in `workspace` references.
    fn read(&self, app: &str, workspace: &str, hash: &str) -> Option<Vec<u8>>;
}

impl<F> Kept for F
where
    F: Fn(&str, &str, &str) -> Option<Vec<u8>> + Send + Sync,
{
    fn read(&self, app: &str, workspace: &str, hash: &str) -> Option<Vec<u8>> {
        self(app, workspace, hash)
    }
}

impl std::fmt::Debug for dyn Kept {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Kept")
    }
}
