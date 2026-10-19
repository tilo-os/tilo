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

    /// Bounded public presentation, bound to the certified person's root key.
    #[cfg(feature = "local-daemons")]
    fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
        None
    }
}

/// Resolve a photo only for the exact canonical root public key. Account names
/// and email addresses are never interpreted as the local person.
#[cfg(feature = "local-daemons")]
pub(crate) fn photo_for(identity: &str) -> Option<runtime_identity::ProfilePhoto> {
    let source = SOURCE.read_recover().clone();
    match source {
        Some(source) => photo_from(source.as_ref(), identity),
        None => photo_from(&RuntimeIdentity, identity),
    }
}

#[cfg(feature = "local-daemons")]
fn photo_from(
    source: &dyn LocalIdentitySource,
    identity: &str,
) -> Option<runtime_identity::ProfilePhoto> {
    // Reject aliases before asking the authenticated source for any metadata.
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let (key, photo) = source.public_photo()?;
    let canonical: String = key.iter().map(|byte| format!("{byte:02x}")).collect();
    (identity == canonical).then_some(photo)
}

/// The local person's own public photo, ready to paint, if they set one:
/// for apps that show the person beside what they wrote (their own mail).
#[must_use]
pub fn own_photo() -> Option<crate::peniko::ImageData> {
    #[cfg(feature = "local-daemons")]
    {
        let source = SOURCE.read_recover().clone();
        let (_, photo) = match source {
            Some(source) => source.public_photo(),
            None => RuntimeIdentity.public_photo(),
        }?;
        Some(crate::widget::avatar::profile_image(&photo))
    }
    #[cfg(not(feature = "local-daemons"))]
    {
        None
    }
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

    fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
        runtime_identity::bound_profile_photo()
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
}

#[cfg(feature = "local-daemons")]
fn default_bound() -> Option<LocalIdentity> {
    RuntimeIdentity::resolve(false)
}

#[cfg(not(feature = "local-daemons"))]
const fn default_bound() -> Option<LocalIdentity> {
    None
}

/// The person's active identity record under
/// `$XDG_CONFIG_HOME/tilo/identity` (else `$HOME/.config/tilo/identity`):
/// the one its `active` selector names, else the legacy `primary.json`.
/// A device set up with more than one identity keeps them as
/// `identity-<key>.json` beside that selector and no `primary.json`; reading
/// only `primary.json` left such a person nameless on their own desktop.
#[cfg(feature = "local-daemons")]
fn config_identity_path() -> Option<std::path::PathBuf> {
    identity_record_in(&crate::host::environment().paths.config_dir()?)
}

/// [`config_identity_path`] under the config directory `config`.
#[cfg(feature = "local-daemons")]
fn identity_record_in(config: &std::path::Path) -> Option<std::path::PathBuf> {
    runtime_identity::resolve_active_identity(&config.join("tilo").join("identity"))
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

#[cfg(all(test, feature = "local-daemons"))]
mod photo_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CertifiedPhoto {
        calls: AtomicUsize,
    }
    impl LocalIdentitySource for CertifiedPhoto {
        fn load(&self) -> Option<LocalIdentity> {
            None
        }
        fn public_photo(&self) -> Option<([u8; 32], runtime_identity::ProfilePhoto)> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some((
                [0x12; 32],
                runtime_identity::ProfilePhoto::new(1, 1, vec![20, 90, 210, 255]).ok()?,
            ))
        }
    }

    #[test]
    fn public_photo_requires_exact_person_key_and_never_queries_for_account_aliases() {
        let source = CertifiedPhoto {
            calls: AtomicUsize::new(0),
        };
        for alias in ["Ada", "ada@example.org", "12", &"AB".repeat(32)] {
            assert!(photo_from(&source, alias).is_none());
        }
        assert_eq!(source.calls.load(Ordering::SeqCst), 0);
        assert!(photo_from(&source, &"13".repeat(32)).is_none());
        let photo = photo_from(&source, &"12".repeat(32)).unwrap();
        assert_eq!(photo.rgba(), &[20, 90, 210, 255]);
        assert!(photo_from(&NoLocalIdentity, &"12".repeat(32)).is_none());
    }
}

#[cfg(all(test, unix, feature = "local-daemons"))]
mod active_record_tests {
    use std::os::unix::fs::PermissionsExt as _;

    fn private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    /// A device set up with more than one identity keeps each as
    /// `identity-<key>.json` beside an `active` selector, with no
    /// `primary.json`: the person's name is read from the active record, so
    /// the owner of the desktop is never nameless on it.
    #[test]
    fn the_active_identity_record_is_read_where_there_is_no_primary() {
        let config = tempfile::tempdir().unwrap();
        let dir = config.path().join("tilo").join("identity");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let record = runtime_identity::public_member_record("Ada", [10, 20, 30], [7; 32]).unwrap();
        private(&dir.join("identity-ada.json"), &record).unwrap();
        private(&dir.join("active"), b"identity-ada\n").unwrap();

        let path = super::identity_record_in(config.path()).expect("the active record");
        assert_eq!(path, dir.join("identity-ada.json"));
        let metadata = runtime_identity::load_metadata_from_path(&path).unwrap();
        assert_eq!(metadata.display_name, "Ada");
    }
}
