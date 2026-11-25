//! Who sits at this device, as far as undo attribution and history authors
//! are concerned.
//!
//! This is the process's one identity boundary, the toolkit's and the collab
//! client's, and it needs neither: a toolkit built without Tilo's model has no
//! source installed and so no local identity. The toolkit reads the person's
//! public identity only through the process's [`LocalIdentitySource`]. The
//! native source, which reads the workspace identity service its launcher bound
//! it to, is the collab client's (`collaboration::local_identity`); a browser
//! page installs one derived from its certified device (the server peer's
//! `DeviceCert` names the user root) with [`install_source`] once certification
//! completes. Without either there is no local identity, and callers fall back
//! to their account-name ladders.

use std::sync::{Arc, RwLock};
use tilo_lock::RwLockRecover as _;

/// The public half of the local person's identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    /// The user's group (root) public key.
    pub group_public_key: [u8; 32],
    /// The name the person chose; empty until they choose one.
    pub display_name: String,
    /// Avatar colour as sRGB bytes.
    pub avatar_color: [u8; 3],
    /// The key of this device, when its identity service certified it: which of the person's
    /// devices is this one.
    pub device_public_key: Option<[u8; 32]>,
}

/// Resolves the local person's identity. Not memoised by callers: a process
/// hosting more than one identity must not be frozen into the first one.
pub trait LocalIdentitySource: Send + Sync {
    fn load(&self) -> Option<LocalIdentity>;

    /// Read the person's identity again now, for this process having just written it: a writer
    /// must not go on reading the name it replaced while it waits for the watch that would have
    /// said so. Nothing where reading fresh costs nothing, as it does for a source that carries
    /// the identity.
    fn refresh(&self) {}
}

static SOURCE: RwLock<Option<Arc<dyn LocalIdentitySource>>> = RwLock::new(None);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Changes whenever the host replaces or revokes this process's identity.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Make `source` the identity of this process, or restore no identity with
/// `None`. Readers see the change on their next [`load`].
pub fn install_source(source: Option<Arc<dyn LocalIdentitySource>>) {
    *SOURCE.write_recover() = source;
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// The local identity from the installed source; none when no host installed
/// one.
pub fn load() -> Option<LocalIdentity> {
    SOURCE.read_recover().clone()?.load()
}

/// Read the person's identity again now, where this process has just written it: the installed
/// source is told, so the name it hands out next is the one kept, not the one it replaced.
pub fn refresh() {
    let installed = SOURCE.read_recover().clone();
    if let Some(source) = installed {
        source.refresh();
    }
}

/// A source with no identity.
pub struct NoLocalIdentity;

impl LocalIdentitySource for NoLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        None
    }
}

/// A fixed identity, for hosts that learn it once (a certified browser
/// device) and tests.
pub struct FixedIdentity(pub LocalIdentity);

impl LocalIdentitySource for FixedIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        Some(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a host installs is what a reader finds, and replacing it moves
    /// the generation a reader watches.
    #[test]
    fn the_installed_source_is_what_load_reads() {
        let _guard = tilo_test_env::lock();
        install_source(Some(Arc::new(FixedIdentity(LocalIdentity {
            group_public_key: [7; 32],
            display_name: "Ada".into(),
            avatar_color: [200, 40, 90],
            device_public_key: None,
        }))));
        assert_eq!(
            load().map(|identity| identity.display_name),
            Some("Ada".to_owned())
        );
        let before = generation();
        install_source(None);
        assert_eq!(load(), None);
        assert!(generation() > before);
    }
}
