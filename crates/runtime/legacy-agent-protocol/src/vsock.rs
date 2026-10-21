//! `AF_VSOCK` streams: the host ↔ guest transport, with no network in between.
//!
//! The standard library has no vsock sockets, so this wraps the four system
//! calls a stream needs. Reads and writes go through the descriptor as a
//! [`File`], which is exactly `read(2)`/`write(2)`.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use crate::Duplex;

/// `VMADDR_CID_ANY`: listen on every context id this side owns.
pub const CID_ANY: u32 = libc::VMADDR_CID_ANY;

fn address(cid: u32, port: u32) -> libc::sockaddr_vm {
    // SAFETY: `sockaddr_vm` is plain old data; all-zero is a valid value and
    // the fields that matter are set below.
    let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    address.svm_family = libc::sa_family_t::try_from(libc::AF_VSOCK).unwrap_or_default();
    address.svm_cid = cid;
    address.svm_port = port;
    address
}

fn socket() -> io::Result<OwnedFd> {
    // SAFETY: `socket` has no memory preconditions; the result is checked.
    let fd = unsafe { libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor this function exclusively owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[allow(clippy::cast_possible_truncation, reason = "a 16-byte struct")]
const ADDRESS_LEN: libc::socklen_t = std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t;

/// A connected vsock stream.
#[derive(Debug)]
pub struct VsockStream {
    file: File,
}

impl VsockStream {
    /// Connect to `port` on context id `cid`. The kernel bounds the connect
    /// itself (two seconds by default); an absent or booting guest fails fast.
    pub fn connect(cid: u32, port: u32) -> io::Result<Self> {
        let fd = socket()?;
        let address = address(cid, port);
        // SAFETY: `address` is a valid `sockaddr_vm` of `ADDRESS_LEN` bytes
        // that outlives the call.
        let result = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                std::ptr::from_ref(&address).cast(),
                ADDRESS_LEN,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            file: File::from(fd),
        })
    }

    /// Bound every later read; `None` waits forever.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let value = timeout.map_or(
            libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            |timeout| libc::timeval {
                tv_sec: libc::time_t::try_from(timeout.as_secs()).unwrap_or(libc::time_t::MAX),
                tv_usec: libc::suseconds_t::from(timeout.subsec_micros().max(1)),
            },
        );
        let length = libc::socklen_t::try_from(std::mem::size_of::<libc::timeval>())
            .map_err(io::Error::other)?;
        // SAFETY: `value` is a valid `timeval` that outlives the call and its
        // size is passed alongside.
        let result = unsafe {
            libc::setsockopt(
                self.file.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&value).cast(),
                length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn shutdown(&self, how: libc::c_int) -> io::Result<()> {
        // SAFETY: `shutdown` on a descriptor this value owns.
        if unsafe { libc::shutdown(self.file.as_raw_fd(), how) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl AsRawFd for VsockStream {
    fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }
}

impl Read for VsockStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Write for VsockStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Duplex for VsockStream {
    fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            file: self.file.try_clone()?,
        })
    }
    fn shutdown_write(&self) -> io::Result<()> {
        self.shutdown(libc::SHUT_WR)
    }
    fn shutdown_both(&self) -> io::Result<()> {
        self.shutdown(libc::SHUT_RDWR)
    }
}

/// A listening vsock socket.
#[derive(Debug)]
pub struct VsockListener {
    fd: OwnedFd,
}

impl VsockListener {
    /// Listen on `port` for any context id this side owns.
    pub fn bind(port: u32) -> io::Result<Self> {
        let fd = socket()?;
        let address = address(CID_ANY, port);
        // SAFETY: as in `connect`.
        let bound = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                std::ptr::from_ref(&address).cast(),
                ADDRESS_LEN,
            )
        };
        if bound != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `listen` on a bound socket this value owns.
        if unsafe { libc::listen(fd.as_raw_fd(), 64) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    /// The next connection and the context id it came from.
    pub fn accept(&self) -> io::Result<(VsockStream, u32)> {
        let mut peer = address(0, 0);
        let mut length = ADDRESS_LEN;
        // SAFETY: `peer` and `length` are valid for writes of the sizes given.
        let fd = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::from_mut(&mut peer).cast(),
                std::ptr::from_mut(&mut length),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is a fresh descriptor this function exclusively owns.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok((
            VsockStream {
                file: File::from(fd),
            },
            peer.svm_cid,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HostMessage, recv, send};

    /// `VMADDR_CID_LOCAL`: this machine's own loopback context.
    const CID_LOCAL: u32 = 1;

    #[test]
    fn a_stream_carries_frames_over_vsock_loopback() {
        // A host without the vsock loopback transport (no `vsock_loopback`
        // module, or no AF_VSOCK in a container) has nothing to test against.
        let port = 40_000 + std::process::id() % 20_000;
        let Ok(listener) = VsockListener::bind(port) else {
            return;
        };
        let Ok(mut client) = VsockStream::connect(CID_LOCAL, port) else {
            return;
        };
        let (mut server, peer) = listener.accept().unwrap();
        assert_eq!(peer, CID_LOCAL);
        server
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        send(&mut client, &HostMessage::Health).unwrap();
        assert_eq!(
            recv::<_, HostMessage>(&mut server).unwrap(),
            Some(HostMessage::Health)
        );
        client.shutdown_write().unwrap();
        assert_eq!(recv::<_, HostMessage>(&mut server).unwrap(), None);
    }
}
