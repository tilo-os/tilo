//! Cooperative native editor capability. Unsupported hosts expose no editor.
//! Transport and native timing live here; the app owns caller authority.
pub use runtime_portal::editor::{Lease, Request, Status, TEXT_BYTES, VERSION};
use std::io;

/// A refused, busy, retired or uncertain edit is never permission to retry stdin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Rejected,
    Busy,
    Retired,
    Unknown,
}

/// A caller checks its current authority immediately around the supplied send.
pub trait Completion: Send {
    /// Send only while the original caller remains admitted.
    /// # Errors
    /// Returns the caller refusal or uncertain native transport failure.
    fn send(&self, send: &mut dyn FnMut() -> io::Result<()>) -> Result<(), Failure>;
    /// Complete from the actual native acknowledgement or classified failure.
    fn finish(self: Box<Self>, result: Result<(), Failure>);
}
#[derive(Clone)]
pub struct Snapshot {
    pub lease: Lease,
    pub can_undo: bool,
    pub can_redo: bool,
    pub transaction: u64,
}
#[cfg(target_os = "linux")]
struct Observe;
#[cfg(target_os = "linux")]
impl Completion for Observe {
    fn send(&self, send: &mut dyn FnMut() -> io::Result<()>) -> Result<(), Failure> {
        send().map_err(|_| Failure::Retired)
    }
    fn finish(self: Box<Self>, _result: Result<(), Failure>) {}
}
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::{Client, Socket};
#[cfg(not(target_os = "linux"))]
mod unsupported;
#[cfg(not(target_os = "linux"))]
pub use unsupported::{Client, Socket};
/// Whether this host can negotiate a cooperative editor descriptor.
pub const fn supported() -> bool {
    cfg!(target_os = "linux")
}
/// Verify the declared PTY master role on a Linux host.
/// # Errors
/// Refuses a descriptor that is not the master side of a PTY.
#[cfg(unix)]
pub fn validate_master(master: &std::fs::File) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let mut number = 0u32;
        // SAFETY: owned descriptor and initialized PTY number output.
        if unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGPTN, &raw mut number) } != 0 {
            return Err(io::Error::other(
                "The ush PTY role is not a master terminal",
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = master;
    Ok(())
}
