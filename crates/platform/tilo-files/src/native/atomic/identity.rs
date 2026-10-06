use crate::native::atomic::native;
use std::fs::File;
use std::io;

/// The native identity of an opened filesystem object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub(crate) inner: native::Identity,
}

/// The native identity of the volume containing an opened filesystem object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeIdentity {
    pub(crate) inner: native::VolumeIdentity,
}

impl FileIdentity {
    /// Query identity through `file`'s retained handle.
    ///
    /// # Errors
    ///
    /// Returns the native metadata error if identity cannot be observed.
    pub fn of(file: &File) -> io::Result<Self> {
        native::Identity::of(file).map(|inner| Self { inner })
    }

    /// Return the volume component of this identity.
    #[must_use]
    pub fn volume(&self) -> &VolumeIdentity {
        // Both public wrappers are transparent semantic containers. Keep the
        // volume value beside the full native identity so equality never drops
        // the inode component.
        &self.inner.public_volume
    }
}
