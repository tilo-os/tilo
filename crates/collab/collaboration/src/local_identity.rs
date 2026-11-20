//! Who sits at this device, as far as undo attribution and history authors
//! are concerned.
//!
//! This is the process's one identity boundary, the toolkit's and the collab
//! client's. The toolkit reads the person's
//! public identity only through the process's [`LocalIdentitySource`]. The
//! native implementation reads the workspace identity service its launcher
//! bound it to (feature `local-daemons`). A browser page installs one derived from its
//! certified device (the server peer's `DeviceCert` names the user root) with
//! [`install_source`] once certification completes; the device key itself
//! stays in the page's ownership and signs through the server client's
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
    fn public_photo(&self) -> Option<([u8; 32], tilo_identity_client::ProfilePhoto)> {
        None
    }

    /// Read the person's identity again now, for this process having just written it: a writer
    /// must not go on reading the name it replaced while it waits for the watch that would have
    /// said so. Nothing where reading fresh costs nothing, as it does for a source that carries
    /// the identity.
    fn refresh(&self) {}
}

/// Resolve a photo only for the exact canonical root public key. Account names
/// and email addresses are never interpreted as the local person.
#[cfg(feature = "local-daemons")]
#[must_use]
pub fn photo_for(identity: &str) -> Option<tilo_identity_client::ProfilePhoto> {
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
) -> Option<tilo_identity_client::ProfilePhoto> {
    // Reject aliases before asking the authenticated source for any metadata.
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return None;
    }
    let (key, photo) = source.public_photo()?;
    let canonical = hex::encode(key);
    (identity == canonical).then_some(photo)
}

/// The local person's own public photo, if they set one: for apps that show
/// the person beside what they wrote (their own mail).
#[cfg(feature = "local-daemons")]
#[must_use]
pub fn own_public_photo() -> Option<tilo_identity_client::ProfilePhoto> {
    let source = SOURCE.read_recover().clone();
    let (_, photo) = match source {
        Some(source) => source.public_photo(),
        None => RuntimeIdentity.public_photo(),
    }?;
    Some(photo)
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

/// Read the person's identity again now, where this process has just written it: the installed
/// source is told, so the name it hands out next is the one kept, not the one it replaced.
pub fn refresh() {
    let installed = SOURCE.read_recover().clone();
    if let Some(source) = installed {
        source.refresh();
    }
}

/// What the person is called: their profile name, when they have one.
#[must_use]
pub fn own_name() -> Option<String> {
    load()
        .map(|identity| identity.display_name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

/// The colour this person chose to wear everywhere (`identity.colour`, as their
/// own card keeps it), `None` while they chose none and every surface derives
/// theirs from their identity.
///
/// Their apps and their shell announce it where they say who they are, and every
/// surface paints them in it, so one person is one colour here, in every app
/// they edit in and on everyone else's screens.
#[must_use]
pub fn own_colour() -> Option<collab_types::presence::PresenceHue> {
    settings_store::identity::COLOUR
        .get()
        .as_deref()
        .and_then(collab_types::presence::PresenceHue::from_id)
}

/// A source with no identity.
pub struct NoLocalIdentity;

impl LocalIdentitySource for NoLocalIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        None
    }
}

/// The workspace identity service the process's launcher bound it to.
#[cfg(feature = "local-daemons")]
pub struct RuntimeIdentity;

#[cfg(feature = "local-daemons")]
impl LocalIdentitySource for RuntimeIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        Self::resolve()
    }

    fn public_photo(&self) -> Option<([u8; 32], tilo_identity_client::ProfilePhoto)> {
        tilo_identity_client::bound_profile_photo()
    }
}

#[cfg(feature = "local-daemons")]
impl RuntimeIdentity {
    /// The person the inherited identity service names.
    ///
    /// Only the service's public fields are read: a name and colour never
    /// reach for the signing secret, which on a desktop with a Secret
    /// Service is a D-Bus round trip (and maybe an unlock prompt). The shell
    /// asks on its broker and network runtimes for every presence frame.
    fn resolve() -> Option<LocalIdentity> {
        let id = bound_metadata()?;
        Some(LocalIdentity {
            group_public_key: id.group_public_key,
            display_name: id.display_name,
            avatar_color: id.avatar_color,
        })
    }
}

#[cfg(feature = "local-daemons")]
fn bound_metadata() -> Option<tilo_identity_client::IdentityMetadata> {
    match tilo_identity_client::load_metadata() {
        Ok(metadata) => Some(metadata),
        Err(runtime_portal::NotAvailable::ServiceFdMissing) => None,
        Err(error) => {
            tracing::warn!(%error, "the bound identity service could not be read; this app runs with no local identity");
            None
        }
    }
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

#[cfg(test)]
mod name_tests {
    use super::*;

    /// Peers learn what to call this person from the name the app announces
    /// on joining: the profile name, never an id in place of one.
    #[test]
    fn an_app_names_its_person_by_their_profile_name() {
        let _guard = tilo_test_env::lock();
        let person = |name: &str| {
            install_source(Some(Arc::new(FixedIdentity(LocalIdentity {
                group_public_key: [7; 32],
                display_name: name.into(),
                avatar_color: [200, 40, 90],
            }))));
        };
        person("  Throwaway test identity ");
        assert_eq!(own_name().as_deref(), Some("Throwaway test identity"));
        person(" ");
        assert_eq!(own_name(), None);
        install_source(None);
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
        fn public_photo(&self) -> Option<([u8; 32], tilo_identity_client::ProfilePhoto)> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some((
                [0x12; 32],
                tilo_identity_client::ProfilePhoto::new(1, 1, vec![20, 90, 210, 255]).ok()?,
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
