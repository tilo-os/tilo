//! Fixed installed retained-helper role over genuine kernel process custody.
//! This role is not a source/file grant or an identity certificate.
#[cfg(target_os = "linux")]
use crate::process_binding::ExecutingPeer;
use std::io;
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    sync::Arc,
};

pub const HELPER_PATH: &str = "/usr/libexec/tilo-prepared-setup";
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
#[cfg(target_os = "linux")]
fn installed_program(path: &str, parent: &str) -> io::Result<File> {
    for path in ["/", "/usr", parent] {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(refused());
        }
    }
    let helper = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = helper.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.mode() & 0o111 == 0
        || metadata.len() == 0
    {
        return Err(refused());
    }
    Ok(helper)
}
/// Opaque root-local role minted from a genuine `ExecutingPeer` and the one
/// protected installed helper image. No PID, program or UID wire constructor.
#[cfg(target_os = "linux")]
pub struct VerifiedHelperPeer {
    peer: Arc<ExecutingPeer>,
    helper: File,
    start: u64,
}
#[cfg(target_os = "linux")]
impl VerifiedHelperPeer {
    /// # Errors
    /// Refuses a non-root verifier, wrong/dead task or changed executable.
    pub fn admit(peer: Arc<ExecutingPeer>) -> io::Result<Self> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || peer.uid() != 0
            || !peer.current()
        {
            return Err(refused());
        }
        let start = peer.start_time()?;
        let role = Self {
            peer,
            helper: installed_program(HELPER_PATH, "/usr/libexec")?,
            start,
        };
        role.check_current()?;
        Ok(role)
    }
    /// # Errors
    /// Every use observes the same held pidfd/start and fixed installed image.
    pub fn check_current(&self) -> io::Result<()> {
        if rustix::process::getuid().as_raw() != 0
            || rustix::process::geteuid().as_raw() != 0
            || self.peer.uid() != 0
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        let named = installed_program(HELPER_PATH, "/usr/libexec")?.metadata()?;
        let held = self.helper.metadata()?;
        let image = File::open(format!("/proc/{}/exe", self.peer.pid()))?.metadata()?;
        let expected = (held.dev(), held.ino());
        if (named.dev(), named.ino()) != expected
            || (image.dev(), image.ino()) != expected
            || !self.peer.current()
            || self.peer.start_time()? != self.start
        {
            return Err(refused());
        }
        Ok(())
    }
    /// Kernel-observed recipient of the retained person proof, never authority
    /// derived from a serialized root-PID claim.
    pub fn peer(&self) -> &ExecutingPeer {
        &self.peer
    }
}
