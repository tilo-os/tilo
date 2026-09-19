#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::{
    CommitGuard, Directory, Identity, VolumeIdentity, publish, validate_component,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{CommitGuard, Directory, Identity, VolumeIdentity, publish};

#[cfg(all(windows, feature = "test-support"))]
pub(crate) use windows::directory_probe::run as run_directory_probe;
