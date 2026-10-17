//! Consuming permission bundle for the root broker's future multi-root admission.
//!
//! This holds approved descriptors and original placements, not snapshots, saved
//! credentials, volume witnesses, or an export receipt. The single-root broker
//! must not treat this bundle alone as an original-source read admission.
use super::*;
mod transferred;
pub use transferred::TransferredHomeRoots;

/// Every mounted location from one exact discovery has a matching platform grant.
/// Only `HomeRootSet::into_granted_roots` can construct this device-local bundle.
pub struct GrantedHomeRoots {
    grants: HomeRootSet,
}

/// Borrowed source capability. Placement is relative to the approved primary
/// Home; the primary root has the empty placement. No ambient path is exposed.
pub struct GrantedRoot<'a> {
    placement: &'a Path,
    file: &'a File,
    kind: RootKind,
    mount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    Directory,
    File,
}

impl HomeRootSet {
    /// Consume a complete permission generation. Missing grants, a live chooser,
    /// changed ancestry, or substituted mounted entries refuse the whole bundle.
    /// This does not enumerate mounted contents or assert their consistency.
    pub fn into_granted_roots(self, cancel: &AtomicBool) -> io::Result<GrantedHomeRoots> {
        self.validate(cancel)?;
        if self.pending.is_some() || self.approved.len() != self.locations.candidates.len() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Every discovered mounted Home location needs its matching grant",
            ));
        }
        for (index, candidate) in self.locations.candidates.iter().enumerate() {
            crate::migration_entry::relative(&candidate.relative)?;
            if !self.approved.contains_key(&index) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            // Discovery stops at each mount. An overlapping placement would
            // therefore be an inconsistent discovery, not an implicit child grant.
            for other in &self.locations.candidates[..index] {
                if candidate.relative.starts_with(&other.relative)
                    || other.relative.starts_with(&candidate.relative)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "Mounted Home placements overlap within one discovery",
                    ));
                }
            }
        }
        self.validate(cancel)?;
        Ok(GrantedHomeRoots { grants: self })
    }
}

impl GrantedHomeRoots {
    pub fn generation(&self) -> uuid::Uuid {
        self.grants.id
    }
    /// Retains the original approved Home, its ancestry and every selected mount.
    /// Ordinary content writes can invalidate these observations; callers must
    /// establish genuine offline/snapshot epochs before exporting any bytes.
    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.grants.validate(cancel)
    }

    /// Root-broker integration seam: all descriptors remain borrowed from this
    /// bundle and are reattested on both sides of the operation. A callback error
    /// never skips the final source check. This does not mint a protected witness.
    pub fn with_roots<T>(
        &self,
        cancel: &AtomicBool,
        operation: impl FnOnce(&[GrantedRoot<'_>]) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate(cancel)?;
        let locations = &self.grants.locations;
        let mut roots = Vec::with_capacity(locations.candidates.len() + 1);
        roots.push(GrantedRoot {
            placement: Path::new(""),
            file: &locations.root.file,
            kind: RootKind::Directory,
            mount: fs_native::snapshot_lease::mount_id(&locations.root.file)?,
        });
        for (index, candidate) in locations.candidates.iter().enumerate() {
            let approved = self.grants.approved.get(&index).ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "Mounted grant is missing")
            })?;
            let (file, kind) = match approved {
                Approved::Directory(pin) => (&pin.file, RootKind::Directory),
                Approved::File(file) => (file, RootKind::File),
            };
            roots.push(GrantedRoot {
                placement: &candidate.relative,
                file,
                kind,
                mount: candidate.mount,
            });
        }
        let result = operation(&roots);
        self.validate(cancel)?;
        result
    }
}

impl GrantedRoot<'_> {
    pub fn placement(&self) -> &Path {
        self.placement
    }
    /// Descriptor of the exact approved entry. Trusted broker code must project
    /// it into its confined namespace before letting a source worker use it.
    pub fn file(&self) -> &File {
        self.file
    }
    pub fn kind(&self) -> RootKind {
        self.kind
    }
    pub fn mount_id(&self) -> u64 {
        self.mount
    }
}
