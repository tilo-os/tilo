//! Who sits at this device, as far as undo attribution and history authors
//! are concerned.
//!
//! This is the toolkit's one identity seam. The toolkit reads the person's
//! public identity only through the process's [`LocalIdentitySource`]. The
//! native implementation reads the runtime identity the launcher bound
//! (`TILO_USER_IDENTITY`), then `$XDG_CONFIG_HOME/tilo/identity/primary.json`
//! (feature `local-daemons`). A browser page installs one derived from its
//! certified device (the relay peer's `DeviceCert` names the user root) with
//! [`install_source`] once certification completes; the device key itself
//! stays in the page's custody and signs through the relay client's
//! `DeviceSigner`, never through the toolkit. Without either there is no
//! local identity, and callers fall back to their account-name ladders.

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
}

/// Resolves the local person's identity. Not memoised by callers: a process
/// hosting more than one identity must not be frozen into the first one.
pub trait LocalIdentitySource: Send + Sync {
    fn load(&self) -> Option<LocalIdentity>;
}

static SOURCE: RwLock<Option<Arc<dyn LocalIdentitySource>>> = RwLock::new(None);
static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Changes whenever the host replaces or revokes this process's identity.
pub fn generation() -> u64 {
    GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Make `source` the identity of this process, or restore the default
/// (the native runtime identity with `local-daemons`, else none) with `None`.
/// Readers see the change on their next [`load`].
pub fn install_source(source: Option<Arc<dyn LocalIdentitySource>>) {
    *SOURCE.write_recover() = source;
    GENERATION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
}

/// The local identity from the installed source, else the native runtime
/// identity (feature `local-daemons`), else none.
pub fn load() -> Option<LocalIdentity> {
    let installed = SOURCE.read_recover().clone();
    match installed {
        Some(source) => source.load(),
        None => default_load(),
    }
}

/// The identity bound to this process: the installed source, else the
/// runtime identity the launcher bound (feature `local-daemons`), else none.
/// Unlike [`load`], never the shell's own `primary.json`: for callers that
/// fall back to an identity of their own when the launcher bound none.
pub fn bound() -> Option<LocalIdentity> {
    let installed = SOURCE.read_recover().clone();
    match installed {
        Some(source) => source.load(),
        None => default_bound(),
    }
}

/// A source with no identity.
pub struct NoLocalIdentity;

impl LocalIdentitySource for NoLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        None
    }
}

/// The runtime identity bound by the launcher, else the shell's
/// `primary.json` for its own unsandboxed in-process apps.
#[cfg(feature = "local-daemons")]
pub struct RuntimeIdentity;

#[cfg(feature = "local-daemons")]
impl LocalIdentitySource for RuntimeIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        Self::resolve(true)
    }
}

#[cfg(feature = "local-daemons")]
impl RuntimeIdentity {
    /// The launcher's identity, else (with `shell_config`) `primary.json`.
    ///
    /// Only the record's public fields are read: a name and colour never
    /// reach for the signing secret, which on a desktop with a Secret
    /// Service is a D-Bus round trip (and maybe an unlock prompt). The shell
    /// asks on its broker and network runtimes for every presence frame.
    #[cfg(unix)]
    fn resolve(shell_config: bool) -> Option<LocalIdentity> {
        let id = runtime_identity::load_metadata().ok().or_else(|| {
            shell_config
                .then(config_identity_path)
                .flatten()
                .and_then(|p| runtime_identity::load_metadata_from_path(&p).ok())
        })?;
        Some(LocalIdentity {
            group_public_key: id.group_public_key,
            display_name: id.display_name,
            avatar_color: id.avatar_color,
        })
    }

    #[cfg(not(unix))]
    fn resolve(_shell_config: bool) -> Option<LocalIdentity> {
        None
    }
}

#[cfg(feature = "local-daemons")]
fn default_bound() -> Option<LocalIdentity> {
    RuntimeIdentity::resolve(false)
}

#[cfg(not(feature = "local-daemons"))]
const fn default_bound() -> Option<LocalIdentity> {
    None
}

/// `$XDG_CONFIG_HOME/tilo/identity/primary.json`, else
/// `$HOME/.config/tilo/identity/primary.json`. Mirrors the resolution in
/// `shell::firstboot::resolve_identity_dir`, which is what writes the file.
#[cfg(all(unix, feature = "local-daemons"))]
fn config_identity_path() -> Option<std::path::PathBuf> {
    let base = crate::host::environment().paths.config_dir()?;
    let p = base.join("tilo").join("identity").join("primary.json");
    p.is_file().then_some(p)
}

#[cfg(feature = "local-daemons")]
fn default_load() -> Option<LocalIdentity> {
    RuntimeIdentity.load()
}

#[cfg(not(feature = "local-daemons"))]
const fn default_load() -> Option<LocalIdentity> {
    None
}

/// A fixed identity, for hosts that learn it once (a certified browser
/// device) and tests.
pub struct FixedIdentity(pub LocalIdentity);

impl LocalIdentitySource for FixedIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        Some(self.0.clone())
    }
}
