//! Where a tunnel's packets enter and leave this machine's network stack.

use std::future::Future;
use std::io;

pub trait PacketDevice: Send + Sync + 'static {
    fn recv<'a>(&'a self, buf: &'a mut [u8])
    -> impl Future<Output = io::Result<usize>> + Send + 'a;
    fn send<'a>(&'a self, packet: &'a [u8]) -> impl Future<Output = io::Result<()>> + Send + 'a;
}

#[cfg(target_os = "linux")]
pub struct TunDevice {
    fd: tokio::io::unix::AsyncFd<std::os::fd::OwnedFd>,
}

#[cfg(target_os = "linux")]
impl TunDevice {
    /// Drive `fd` without blocking. Must be called inside a Tokio runtime.
    pub fn new(fd: std::os::fd::OwnedFd) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        // SAFETY: plain flag changes on a descriptor this value owns.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            // SAFETY: as above.
            || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` moves into the AsyncFd and nothing else holds it, so
        // the TUN stays open until the AsyncFd deregisters it on drop.
        let fd = unsafe { tokio::io::unix::AsyncFd::register(fd) }?;
        Ok(Self { fd })
    }
}

#[cfg(target_os = "linux")]
impl PacketDevice for TunDevice {
    async fn recv<'a>(&'a self, buf: &'a mut [u8]) -> io::Result<usize> {
        use std::os::fd::AsRawFd as _;
        loop {
            let mut guard = self.fd.readable().await?;
            let read = guard.try_io(|fd| {
                // SAFETY: `buf` is writable for its whole length.
                let read =
                    unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                usize::try_from(read).map_err(|_| io::Error::last_os_error())
            });
            if let Ok(read) = read {
                return read;
            }
        }
    }

    async fn send<'a>(&'a self, packet: &'a [u8]) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        loop {
            let mut guard = self.fd.writable().await?;
            let written = guard.try_io(|fd| {
                // SAFETY: `packet` is readable for its whole length.
                let written =
                    unsafe { libc::write(fd.as_raw_fd(), packet.as_ptr().cast(), packet.len()) };
                usize::try_from(written).map_err(|_| io::Error::last_os_error())
            });
            if let Ok(written) = written {
                return written.map(drop);
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing {
    use std::io;

    use tokio::sync::{Mutex, mpsc};

    use super::PacketDevice;

    /// A device whose network stack is the test: what the test sends with
    /// [`Stack::inject`] enters the tunnel, and what the tunnel hands the
    /// stack arrives at [`Stack::delivered`].
    pub struct ChannelDevice {
        inbound: Mutex<mpsc::Receiver<Vec<u8>>>,
        outbound: mpsc::Sender<Vec<u8>>,
    }

    pub struct Stack {
        inject: mpsc::Sender<Vec<u8>>,
        delivered: mpsc::Receiver<Vec<u8>>,
    }

    impl Stack {
        /// Send `packet` into the tunnel, as the network stack would.
        pub async fn inject(&self, packet: Vec<u8>) {
            let _ = self.inject.send(packet).await;
        }

        pub async fn delivered(&mut self) -> Option<Vec<u8>> {
            self.delivered.recv().await
        }
    }

    #[must_use]
    pub fn channel_device() -> (ChannelDevice, Stack) {
        let (inject, inbound) = mpsc::channel(64);
        let (outbound, delivered) = mpsc::channel(64);
        (
            ChannelDevice {
                inbound: Mutex::new(inbound),
                outbound,
            },
            Stack { inject, delivered },
        )
    }

    impl PacketDevice for ChannelDevice {
        async fn recv<'a>(&'a self, buf: &'a mut [u8]) -> io::Result<usize> {
            let packet =
                self.inbound.lock().await.recv().await.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::BrokenPipe, "the stack is gone")
                })?;
            let length = packet.len().min(buf.len());
            if let (Some(to), Some(from)) = (buf.get_mut(..length), packet.get(..length)) {
                to.copy_from_slice(from);
            }
            Ok(length)
        }

        async fn send<'a>(&'a self, packet: &'a [u8]) -> io::Result<()> {
            self.outbound
                .send(packet.to_vec())
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the stack is gone"))
        }
    }
}
