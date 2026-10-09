//! The canonical control connection, shared by the CLI and lifecycle consumer.
use std::{
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const ROUND_TRIP_TIMEOUT: Duration = Duration::from_secs(60);

/// One connection to the existing semantic control socket. No alternate protocol.
pub struct ControlClient {
    stream: UnixStream,
    pending: Vec<u8>,
    timeout: Duration,
}

impl ControlClient {
    /// Connect using the same socket path as the ordinary CLI.
    /// # Errors
    /// Returns the socket connection error.
    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self {
            stream: UnixStream::connect(path)?,
            pending: Vec::new(),
            timeout: ROUND_TRIP_TIMEOUT,
        })
    }

    /// Exchange one bounded JSON line with an overall read/write deadline.
    /// # Errors
    /// Refuses oversized/multiline requests, oversized/truncated/invalid replies,
    /// or a socket that does not complete within the deadline.
    pub fn exchange(&mut self, request: &str) -> io::Result<serde_json::Value> {
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(|| invalid("control deadline overflow"))?;
        self.exchange_before(request, deadline)
    }

    /// Exchange using a caller's whole-operation deadline as well as the
    /// ordinary per-round-trip limit. This keeps multi-call semantic actions
    /// bounded without extending the CLI's existing timeout.
    /// # Errors
    /// Returns the same framing, transport and timeout failures as exchange.
    pub fn exchange_before(
        &mut self,
        request: &str,
        deadline: Instant,
    ) -> io::Result<serde_json::Value> {
        if request.len() > MAX_REQUEST_BYTES || request.contains(['\n', '\r']) {
            return Err(invalid("control request exceeds its line bound"));
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(|| invalid("control deadline overflow"))?
            .min(deadline);
        let mut line = request.as_bytes().to_vec();
        line.push(b'\n');
        while !line.is_empty() {
            self.stream.set_write_timeout(Some(remaining(deadline)?))?;
            let written = self.stream.write(&line)?;
            if written == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            line.drain(..written);
        }
        let mut scanned = 0;
        loop {
            let unchecked = self
                .pending
                .get(scanned..)
                .ok_or_else(|| invalid("control frame cursor is invalid"))?;
            if let Some(offset) = unchecked.iter().position(|byte| *byte == b'\n') {
                let end = scanned
                    .checked_add(offset)
                    .ok_or_else(|| invalid("control frame cursor overflow"))?;
                if end > MAX_RESPONSE_BYTES {
                    return Err(invalid("control response exceeds its line bound"));
                }
                let line: Vec<_> = self.pending.drain(..=end).collect();
                return serde_json::from_slice(&line)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
            }
            if self.pending.len() > MAX_RESPONSE_BYTES {
                return Err(invalid("control response exceeds its line bound"));
            }
            scanned = self.pending.len();
            self.stream.set_read_timeout(Some(remaining(deadline)?))?;
            let mut bytes = [0; 8192];
            let count = self.stream.read(&mut bytes)?;
            if count == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            let received = bytes
                .get(..count)
                .ok_or_else(|| invalid("control read length is invalid"))?;
            self.pending.extend_from_slice(received);
        }
    }

    /// Revalidate the original server process and executed inode on this socket.
    /// # Errors
    /// Refuses a departed process, changed credentials, reused PID or exec.
    #[cfg(target_os = "linux")]
    pub fn revalidate_peer(&self, original: &KernelPeer) -> io::Result<()> {
        let current = self.observe_peer()?;
        if (
            current.pid,
            current.uid,
            current.gid,
            current.start_time,
            current.executable_dev,
            current.executable_inode,
        ) != (
            original.pid,
            original.uid,
            original.gid,
            original.start_time,
            original.executable_dev,
            original.executable_inode,
        ) {
            return Err(invalid("control server custody changed"));
        }
        Ok(())
    }

    /// Observe the kernel process owning this connected socket, including its
    /// open executable inode. Caller policy must join it to the owner and
    /// immutable selected host; JSON returned by the server is not authority.
    /// # Errors
    /// Refuses missing kernel credentials, inaccessible executables or process
    /// lifetime changes during observation.
    #[cfg(target_os = "linux")]
    pub fn observe_peer(&self) -> io::Result<KernelPeer> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: an all-zero ucred is a valid output buffer for getsockopt.
        let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
        let mut length = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>())
            .map_err(|_| invalid("kernel credential size overflow"))?;
        // SAFETY: credentials and length are valid writable objects; getsockopt
        // writes no more than length bytes from this connected socket.
        let result = unsafe {
            libc::getsockopt(
                self.stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&raw mut credentials).cast(),
                &raw mut length,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if usize::try_from(length).ok() != Some(std::mem::size_of::<libc::ucred>()) {
            return Err(invalid("kernel peer credentials are incomplete"));
        }
        let pid =
            u32::try_from(credentials.pid).map_err(|_| invalid("kernel peer PID is invalid"))?;
        if pid == 0 {
            return Err(invalid("kernel peer PID is missing"));
        }
        let start_time = start_time(pid)?;
        let executable = std::fs::File::open(format!("/proc/{pid}/exe"))?;
        let metadata = executable.metadata()?;
        if start_time != self::start_time(pid)? {
            return Err(invalid("kernel peer lifetime changed"));
        }
        Ok(KernelPeer {
            pid,
            uid: credentials.uid,
            gid: credentials.gid,
            start_time,
            executable_dev: metadata.dev(),
            executable_inode: metadata.ino(),
            executable,
        })
    }
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|value| !value.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

/// Kernel custody of the same control connection. The executable descriptor
/// continues to name the running inode after a userspace path is replaced.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct KernelPeer {
    pub pid: u32,
    pub uid: u32,
    pub gid: u32,
    pub start_time: u64,
    pub executable_dev: u64,
    pub executable_inode: u64,
    pub executable: std::fs::File,
}

#[cfg(target_os = "linux")]
fn start_time(pid: u32) -> io::Result<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(22 - 3))
        .and_then(|field| field.parse().ok())
        .ok_or_else(|| invalid("kernel peer start time is unavailable"))
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::{fs::MetadataExt as _, net::UnixListener};

    fn connection(
        reply: Vec<u8>,
    ) -> io::Result<(
        ControlClient,
        std::thread::JoinHandle<io::Result<()>>,
        tempfile::TempDir,
    )> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("control.sock");
        let server = UnixListener::bind(&path)?;
        let worker = std::thread::spawn(move || -> io::Result<()> {
            let (mut stream, _) = server.accept()?;
            let mut request = [0; 4096];
            if stream.read(&mut request)? != 0 {
                let _ = stream.write_all(&reply);
            }
            Ok(())
        });
        Ok((ControlClient::connect(&path)?, worker, directory))
    }

    #[test]
    fn ordinary_exchange_observes_actual_kernel_peer_and_running_inode() {
        let (mut client, worker, _directory) = connection(b"{\"ok\":true}\n".to_vec()).unwrap();
        let peer = client.observe_peer().unwrap();
        assert_eq!(peer.pid, std::process::id());
        // SAFETY: getuid/getgid have no preconditions and only read credentials.
        assert_eq!((peer.uid, peer.gid), unsafe {
            (libc::getuid(), libc::getgid())
        });
        let running = std::fs::File::open("/proc/self/exe")
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!(
            (peer.executable_dev, peer.executable_inode),
            (running.dev(), running.ino())
        );
        assert_eq!(peer.start_time, start_time(std::process::id()).unwrap());
        assert_eq!(
            client.exchange("{\"op\":\"state_dump\"}").unwrap(),
            serde_json::json!({"ok": true})
        );
        client.revalidate_peer(&peer).unwrap();
        let mut forged = peer;
        forged.start_time = forged.start_time.checked_add(1).unwrap();
        assert!(client.revalidate_peer(&forged).is_err());
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn ordinary_exchange_refuses_oversized_truncated_and_multiline_frames() {
        for reply in [
            vec![b'x'; MAX_RESPONSE_BYTES + 1],
            b"{\"ok\":true}".to_vec(),
            b"invalid\n".to_vec(),
        ] {
            let (mut client, worker, _directory) = connection(reply).unwrap();
            assert!(client.exchange("{}").is_err());
            drop(client);
            worker.join().unwrap().unwrap();
        }
        let (mut client, worker, _directory) = connection(Vec::new()).unwrap();
        assert!(client.exchange("{}\n{}").is_err());
        assert!(client.exchange(&"x".repeat(MAX_REQUEST_BYTES + 1)).is_err());
        drop(client);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn ordinary_exchange_deadline_bounds_a_dribbling_response() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("control.sock");
        let server = UnixListener::bind(&path).unwrap();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = server.accept().unwrap();
            let mut request = [0; 64];
            assert_ne!(stream.read(&mut request).unwrap(), 0);
            for _ in 0..100 {
                if stream.write_all(b" ").is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        });
        let mut client = ControlClient::connect(&path).unwrap();
        let start = Instant::now();
        let deadline = start.checked_add(Duration::from_millis(20)).unwrap();
        let error = client.exchange_before("{}", deadline).unwrap_err();
        assert!(matches!(
            error.kind(),
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
        drop(client);
        worker.join().unwrap();
    }
}
