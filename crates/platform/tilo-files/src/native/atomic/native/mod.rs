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

#[cfg(all(windows, feature = "test-support"))]
pub(crate) use windows::directory_probe::run as run_directory_probe;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) use unix::publish_policy;
