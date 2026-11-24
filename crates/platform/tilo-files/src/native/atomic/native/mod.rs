#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::{
    CommitGuard, Directory, FilePolicy, Identity, VolumeIdentity, publish, validate_component,
};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub(crate) use windows::{CommitGuard, Directory, Identity, VolumeIdentity, publish};

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use unix::publish_policy;

#[cfg(target_os = "linux")]
pub(crate) use unix::publish_atomic_save;
