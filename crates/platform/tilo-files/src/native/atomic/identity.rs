use crate::native::atomic::native;
use std::fs::File;
use std::io;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub(crate) inner: native::Identity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumeIdentity {
    pub(crate) inner: native::VolumeIdentity,
}

impl FileIdentity {
    pub fn of(file: &File) -> io::Result<Self> {
        native::Identity::of(file).map(|inner| Self { inner })
    }

    #[must_use]
    pub fn volume(&self) -> &VolumeIdentity {
        // Both public wrappers are transparent semantic containers: the volume value is kept beside
        // the full native identity.
        &self.inner.public_volume
    }
}
