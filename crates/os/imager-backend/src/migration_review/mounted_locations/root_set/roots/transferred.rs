//! Broker reconstruction of actual transferred inputs, never platform-ready state.
use super::*;
use crate::{migration_entry::EntryKind, migration_store::NativePath};

pub struct TransferredHomeRoots {
    locations: Arc<MountedHomeLocations>,
    files: Vec<File>,
}
impl TransferredHomeRoots {
    /// Root-only, after existing HostChannel peer admission. Metadata declarations
    /// select candidates only: actual descriptors must match every current named
    /// candidate and its kind. No source owner becomes worker credentials.
    pub fn admit(
        locations: Arc<MountedHomeLocations>,
        inputs: Vec<(NativePath, EntryKind, File)>,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0
            || inputs.is_empty()
            || inputs.len() != locations.candidates.len()
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        locations.validate(cancel)?;
        let mut files = Vec::with_capacity(inputs.len());
        for ((placement, kind, file), candidate) in inputs.into_iter().zip(&locations.candidates) {
            let path = placement.decode().map_err(io::Error::other)?;
            crate::migration_entry::relative(&path)?;
            let metadata = Metadata::from_file(&file)?;
            let directory = metadata.is_dir();
            if path != candidate.relative
                || !(directory || metadata.is_file())
                || directory != (kind == EntryKind::Directory)
                || Fact::capture(&metadata)? != candidate.fact
                || fs_native::snapshot_lease::mount_id(&file)? != candidate.mount
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Transferred mounted entry differs from actual discovered candidate",
                ));
            }
            files.push(file);
        }
        locations.validate(cancel)?;
        Ok(Self { locations, files })
    }
    pub fn with_roots<T>(
        &self,
        cancel: &AtomicBool,
        operation: impl FnOnce(&[GrantedRoot<'_>]) -> io::Result<T>,
    ) -> io::Result<T> {
        self.locations.validate(cancel)?;
        let mut roots = Vec::with_capacity(self.files.len() + 1);
        roots.push(GrantedRoot {
            placement: Path::new(""),
            file: &self.locations.root.file,
            kind: RootKind::Directory,
            mount: fs_native::snapshot_lease::mount_id(&self.locations.root.file)?,
        });
        for (candidate, file) in self.locations.candidates.iter().zip(&self.files) {
            if Fact::capture(&Metadata::from_file(file)?)? != candidate.fact
                || fs_native::snapshot_lease::mount_id(file)? != candidate.mount
            {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            roots.push(GrantedRoot {
                placement: &candidate.relative,
                file,
                kind: if file.metadata()?.is_dir() {
                    RootKind::Directory
                } else {
                    RootKind::File
                },
                mount: candidate.mount,
            });
        }
        let result = operation(&roots);
        self.locations.validate(cancel)?;
        result
    }
}
