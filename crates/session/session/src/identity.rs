//! The identity registry, kept current by a worker: the session's loop never
//! waits on the disk to learn which identity each room presents.
//!
//! The session reads it once at startup, then a worker reads it again
//! whenever a directory it lives in changes: a reconcile lists and parses
//! every identity record, so it runs on the worker
//! ([`tilo_identity_host::registry::IdentityRegistryStore::load_reconciled_prompt`])
//! and the loop is handed the registry it produced
//! ([`tilo_identity_host::registry::IdentityRegistryStore::load_reconciled_cached`]).

/// Read which identity each room presents at startup, and keep it current
/// from then on.
///
/// Called before the session builds what reads identity, so nothing is served
/// the directory's state from before the first reconcile: the first one is
/// this, on this thread, before the loop starts.
pub fn install() {
    match tilo_identity_host::registry::IdentityRegistryStore::from_env() {
        Ok(store) => {
            if let Err(error) = store.load_reconciled_prompt() {
                tracing::warn!(%error, "cannot read the identity registry at startup");
            }
        }
        Err(error) => tracing::warn!(%error, "the identity registry is not at hand"),
    }
}
