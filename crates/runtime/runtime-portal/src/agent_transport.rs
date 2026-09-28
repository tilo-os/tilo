//! Transports a headless agent uses to reach the collaboration broker.
//!
//! Both return a connected descriptor for the toolkit's
//! `CollaborationManager::for_*_fd` constructors, which take ownership of it.

use std::io;
use std::path::Path;

use crate::RawFd;

/// Claim the launcher's managed-agent collaboration descriptor
/// (`TILO_SVC_COLLAB_FD`) and finish the launcher's kernel-credential
/// attestation on it before any Cap'n Proto traffic.
///
/// The descriptor can be claimed once per process.
///
/// # Errors
///
/// Returns an error when the variable is missing or invalid, the descriptor
/// was already claimed, the attestation exchange fails, or the launcher
/// refuses the process binding.
pub fn claim_managed_agent_fd() -> io::Result<RawFd> {
    imp::claim_managed_agent_fd()
}

/// Connect to an explicit collaboration endpoint socket, for headless
/// diagnostics that run outside the launcher but exercise the production
/// protocol.
///
/// # Errors
///
/// Returns the socket connection error.
pub fn connect_endpoint(path: &Path) -> io::Result<RawFd> {
    imp::connect_endpoint(path)
}

#[cfg(unix)]
mod imp {
    use std::io::{self, Read as _, Write as _};
    use std::os::fd::{FromRawFd as _, IntoRawFd as _};
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::RawFd;

    pub fn claim_managed_agent_fd() -> io::Result<RawFd> {
        static CLAIMED: AtomicBool = AtomicBool::new(false);
        let raw: i32 = std::env::var("TILO_SVC_COLLAB_FD")
            .map_err(io::Error::other)?
            .parse()
            .map_err(io::Error::other)?;
        if raw < 3 {
            return Err(io::Error::other("invalid managed collaboration descriptor"));
        }
        CLAIMED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::Error::other("managed descriptor already consumed"))?;
        // SAFETY: the managed launcher transfers this sole descriptor owner
        // once, and `CLAIMED` lets exactly one caller take it.
        let mut stream = unsafe { UnixStream::from_raw_fd(raw) };
        complete_managed_handshake(&mut stream)?;
        Ok(stream.into_raw_fd())
    }

    pub fn connect_endpoint(path: &Path) -> io::Result<RawFd> {
        Ok(UnixStream::connect(path)?.into_raw_fd())
    }

    /// Finish launcher kernel-credential attestation before starting Cap'n Proto.
    pub(super) fn complete_managed_handshake(stream: &mut UnixStream) -> io::Result<()> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        let mut challenge = [0; 32];
        stream.read_exact(&mut challenge)?;
        stream.write_all(&challenge)?;
        let mut ack = [0];
        stream.read_exact(&mut ack)?;
        if ack != [1] {
            return Err(io::Error::other("managed process binding refused"));
        }
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn managed_connection_requires_exact_ready_ack_before_rpc() {
            for acknowledgement in [0, 1] {
                let (mut host, mut app) = UnixStream::pair().unwrap();
                let worker = std::thread::spawn(move || {
                    host.write_all(&[37; 32]).unwrap();
                    let mut answer = [0; 32];
                    host.read_exact(&mut answer).unwrap();
                    assert_eq!(answer, [37; 32]);
                    host.write_all(&[acknowledgement]).unwrap();
                    if acknowledgement == 1 {
                        let mut value = [0];
                        host.read_exact(&mut value).unwrap();
                        assert_eq!(value, [9]);
                    }
                });
                let result = complete_managed_handshake(&mut app);
                assert_eq!(result.is_ok(), acknowledgement == 1);
                if result.is_ok() {
                    app.write_all(&[9]).unwrap();
                }
                worker.join().unwrap();
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::path::Path;

    use crate::RawFd;

    pub fn claim_managed_agent_fd() -> io::Result<RawFd> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "managed agent descriptors need Unix sockets",
        ))
    }

    pub fn connect_endpoint(_path: &Path) -> io::Result<RawFd> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "collaboration endpoints need Unix sockets",
        ))
    }
}
