//! The native local identity: the source that reads the workspace identity
//! service the process's launcher bound it to (`local-daemons`).
//!
//! A toolkit built without Tilo's model has no source installed and so no local
//! identity, and falls back to its account-name ladder. A page installs a source
//! derived from its certified device; a native app process installs none, and
//! reads the identity service it inherited instead. The person's public photo
//! travels with that source: the plain boundary hands out the identity, and a
//! source that carries the photo says so with [`PhotoSource`].

use std::sync::{Arc, RwLock};

use tilo_identity_client::ProfilePhoto;
use tilo_local_identity::{
    FixedIdentity, LocalIdentity, LocalIdentitySource as Boundary, NoLocalIdentity,
};
use tilo_lock::RwLockRecover as _;

/// A local identity source that also carries the person's public photo, bound
/// to their certified root key. A native source has one; a fixed source, a page
/// installs, does not.
pub trait PhotoSource: Boundary {
    fn public_photo(&self) -> Option<([u8; 32], ProfilePhoto)>;
}

/// The installed source, for the photo apps show beside the person's own words.
static PHOTO_SOURCE: RwLock<Option<Arc<dyn PhotoSource>>> = RwLock::new(None);

/// The plain boundary of a source that carries a photo: what the process-wide
/// registry holds, so a reader that knows no photo still reads the identity.
struct PlainBoundary(Arc<dyn PhotoSource>);

impl Boundary for PlainBoundary {
    fn load(&self) -> Option<LocalIdentity> {
        self.0.load()
    }

    fn refresh(&self) {
        self.0.refresh();
    }
}

/// Make `source` the identity of this process, or restore no identity with
/// `None`. Readers see the change on their next [`load`].
pub fn install_source(source: Option<Arc<dyn PhotoSource>>) {
    let boundary = source
        .as_ref()
        .map(|source| Arc::new(PlainBoundary(Arc::clone(source))) as Arc<dyn Boundary>);
    *PHOTO_SOURCE.write_recover() = source;
    tilo_local_identity::install_source(boundary);
}

/// The local identity from the installed source, else the identity service the
/// launcher bound (feature `local-daemons`), else none.
pub fn load() -> Option<LocalIdentity> {
    tilo_local_identity::load().or_else(|| RuntimeIdentity.load())
}

/// Resolve a photo only for the exact canonical root public key. Account names
/// and email addresses are never interpreted as the local person.
#[must_use]
pub fn photo_for(identity: &str) -> Option<ProfilePhoto> {
    match PHOTO_SOURCE.read_recover().clone() {
        Some(source) => photo_from(source.as_ref(), identity),
        None => photo_from(&RuntimeIdentity, identity),
    }
}

fn photo_from(source: &dyn PhotoSource, identity: &str) -> Option<ProfilePhoto> {
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
#[must_use]
pub fn own_public_photo() -> Option<ProfilePhoto> {
    let source = PHOTO_SOURCE.read_recover().clone();
    let (_, photo) = match source {
        Some(source) => source.public_photo(),
        None => RuntimeIdentity.public_photo(),
    }?;
    Some(photo)
}

impl PhotoSource for FixedIdentity {
    fn public_photo(&self) -> Option<([u8; 32], ProfilePhoto)> {
        None
    }
}

impl PhotoSource for NoLocalIdentity {
    fn public_photo(&self) -> Option<([u8; 32], ProfilePhoto)> {
        None
    }
}

/// The workspace identity service the process's launcher bound it to.
pub struct RuntimeIdentity;

impl Boundary for RuntimeIdentity {
    fn load(&self) -> Option<LocalIdentity> {
        Self::resolve()
    }
}

impl PhotoSource for RuntimeIdentity {
    fn public_photo(&self) -> Option<([u8; 32], ProfilePhoto)> {
        tilo_identity_client::bound_profile_photo()
    }
}

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
            device_public_key: id.device_public_key,
        })
    }
}

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

#[cfg(test)]
mod photo_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CertifiedPhoto {
        calls: AtomicUsize,
    }
    impl Boundary for CertifiedPhoto {
        fn load(&self) -> Option<LocalIdentity> {
            None
        }
    }
    impl PhotoSource for CertifiedPhoto {
        fn public_photo(&self) -> Option<([u8; 32], ProfilePhoto)> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Some((
                [0x12; 32],
                ProfilePhoto::new(1, 1, vec![20, 90, 210, 255]).ok()?,
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
