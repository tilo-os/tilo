//! Authenticated original-user capture host and descriptor-bound projection.
//! The frontend can revoke, acknowledge and read events; source/output aliases,
//! sockets, memfds and proof constructors remain private to the backend.
use super::{ProjectionReceipt, process::Incarnation};
use std::{fs::File, io, path::Path, process::ExitStatus};

#[cfg(target_os = "linux")]
mod host;
#[cfg(target_os = "linux")]
mod native;
#[cfg(not(target_os = "linux"))]
mod unsupported;

/// Fixed native helper entrypoint, not a caller-supplied launch mode.
pub const ARG: &str = "--tilo-home-capture-host";

/// A live exact host, before or after its durably journalled handoff.
pub struct HostSession {
    #[cfg(target_os = "linux")]
    inner: native::Session,
    #[cfg(not(target_os = "linux"))]
    never: std::convert::Infallible,
}

/// Cloneable revocation and child-reaping handle, without socket access.
#[derive(Clone)]
pub struct HostProcess {
    #[cfg(target_os = "linux")]
    inner: std::sync::Arc<native::Process>,
    #[cfg(not(target_os = "linux"))]
    never: std::convert::Infallible,
}

/// Read-only event stream. It cannot be converted to a raw descriptor or used
/// to inject acknowledgments into the authenticated control channel.
pub struct Events {
    #[cfg(target_os = "linux")]
    inner: std::os::unix::net::UnixStream,
    #[cfg(not(target_os = "linux"))]
    never: std::convert::Infallible,
}

impl HostSession {
    /// Starts the fixed native helper mode, drops privileged credentials and
    /// authenticates the exact child before any source descriptor can cross.
    pub fn spawn(executable: &Path, uid: u32, output: bool) -> io::Result<Self> {
        Self::spawn_native(executable, uid, output)
    }

    pub fn process(&self) -> HostProcess {
        self.process_native()
    }

    pub fn identity(&self) -> Incarnation {
        self.identity_native()
    }

    /// Persist this receipt with the host identity BEFORE calling `handoff`.
    pub fn projection_receipt(&self) -> ProjectionReceipt {
        self.receipt_native()
    }

    /// Projects approved descriptors into the authenticated private namespace,
    /// seals the request, and transfers the bundle in its fixed protocol order.
    /// No projected file or alias path is returned to the frontend.
    pub fn handoff(
        &mut self,
        source: &File,
        output: Option<&File>,
        scratch: (&File, u64),
        request: &[u8],
        operation: &str,
    ) -> io::Result<()> {
        self.handoff_native(source, output, scratch, request, operation)
    }

    pub fn events(&mut self) -> io::Result<Events> {
        self.events_native()
    }

    pub fn acknowledge(&mut self) -> io::Result<()> {
        self.acknowledge_native()
    }
}

impl HostProcess {
    pub fn revoke(&self) -> io::Result<()> {
        self.revoke_native()
    }
    pub fn try_wait(&self) -> io::Result<Option<ExitStatus>> {
        self.try_wait_native()
    }
    pub fn wait(&self) -> io::Result<ExitStatus> {
        self.wait_native()
    }
    /// Only for failed admission, before a worker launch is acknowledged.
    pub fn abort_unlaunched(&self) -> io::Result<()> {
        self.abort_native()
    }
}

impl io::Read for Events {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read_native(buffer)
    }
}
