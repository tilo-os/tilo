//! The legacy JSON collaboration socket: `u32` little-endian length prefix,
//! then a `serde_json` body — `CollabRequest` outbound, `CollabEvent`
//! inbound. Mirrors `ui::collaboration` (the app side) and
//! `shell::shell::collab_broker` (the broker side) byte for byte.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{CollabEvent, CollabRequest};

/// Where the compositor binds the JSON broker in a native session:
/// `$QUICK_COLLAB_SOCK`, else `$XDG_RUNTIME_DIR/qos/services/collab-json.sock`
/// — the same resolution `shell::compositor::native` uses to bind it.
pub fn default_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("QUICK_COLLAB_SOCK") {
        return PathBuf::from(p);
    }
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = unsafe { libc_getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        });
    runtime.join("qos").join("services").join("collab-json.sock")
}

// Avoid a `libc` dependency for one call.
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// What the kernel says a descriptor names, as a string — `socket:[N]`
/// for a socket, a path for a file.
///
/// Read out of `/proc/self/fd` rather than `fstat`ed because the answer
/// wanted here is an *identity that can be compared and printed*, and
/// this gives both without a `libc` dependency or a `struct stat`.
fn what_the_descriptor_names(fd: std::os::fd::RawFd) -> Option<String> {
    std::fs::read_link(format!("/proc/self/fd/{fd}"))
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// One connection to the broker.
pub struct Link {
    stream: UnixStream,
    rbuf: Vec<u8>,
    /// What this connection's descriptor named when it was opened.
    ///
    /// A read of zero says the far end has gone. It says that whether
    /// the far end really went, or whether this descriptor stopped being
    /// the connection while nobody was looking — anything in this
    /// process that closes a descriptor number it no longer owns frees
    /// it for the next `open`, and a read on whatever took it can return
    /// zero just as readily. The two are the same `Ok(0)` here and they
    /// are opposite diagnoses: one is the broker's problem and one is
    /// this process's.
    ///
    /// So the answer is taken at connect and compared at the hang-up.
    /// Cheap, and it costs nothing on the path that matters — it is read
    /// once when the link is made and once when it ends.
    opened_on: Option<String>,
}

impl Link {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to the collaboration broker at {}", path.display()))?;
        let opened_on = what_the_descriptor_names(stream.as_raw_fd());
        // Said here so the two halves of a session's logs can be joined
        // up. The broker records the pid on the other end of every
        // connection it accepts; without this there was nothing on this
        // side to match it against, and "is the peer in that log this
        // process" had to be inferred from timestamps.
        tracing::info!(
            pid = std::process::id(),
            fd = stream.as_raw_fd(),
            on = opened_on.as_deref().unwrap_or("unknown"),
            "dialled the collaboration broker"
        );
        Ok(Self {
            stream,
            rbuf: Vec::new(),
            opened_on,
        })
    }

    /// Write one request. Blocking: a full-document snapshot can be large
    /// and must not be truncated by a would-block.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        let body = serde_json::to_vec(req).context("encode request")?;
        self.stream.set_nonblocking(false)?;
        self.stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .and_then(|_| self.stream.write_all(&body))
            .context("write to the broker")?;
        Ok(())
    }

    /// Read the next event, waiting at most `timeout`. `Ok(None)` on
    /// timeout; `Err` once the broker has gone away.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        loop {
            if let Some(frame) = self.take_frame() {
                return Ok(Some(serde_json::from_slice(&frame).context("decode event")?));
            }
            self.stream.set_nonblocking(false)?;
            self.stream.set_read_timeout(Some(timeout))?;
            let mut chunk = [0u8; 64 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => anyhow::bail!(self.why_the_read_ended()),
                Ok(n) => self.rbuf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    // A partial frame may be buffered; only a whole one is
                    // an event, so report "nothing yet".
                    return Ok(None);
                }
                Err(e) => return Err(e).context("read from the broker"),
            }
        }
    }

    /// Say what a zero-length read actually means for *this* descriptor.
    ///
    /// Named separately from the bail so the two diagnoses are written
    /// down side by side. The wording of the ordinary case is unchanged
    /// and deliberately so: it is what the harnesses grep for.
    fn why_the_read_ended(&self) -> String {
        let fd = self.stream.as_raw_fd();
        let now = what_the_descriptor_names(fd);
        match (&self.opened_on, &now) {
            (Some(opened), Some(current)) if opened != current => format!(
                "this connection's descriptor is no longer the socket it was opened \
                 on: fd {fd} was {opened} and is now {current}. The broker did not \
                 close anything — something in this process closed or replaced the \
                 descriptor, and the read returned nothing because whatever took the \
                 number had nothing to give"
            ),
            (Some(_), None) => format!(
                "this connection's descriptor is no longer open at all: fd {fd} was \
                 {} and now names nothing. Something in this process closed it",
                self.opened_on.as_deref().unwrap_or("unknown"),
            ),
            // Same descriptor, same socket, and it read zero: the far end
            // really did go away. The original wording, unchanged.
            _ => "the broker closed the connection".to_string(),
        }
    }

    fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.rbuf.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]]) as usize;
        if self.rbuf.len() < 4 + len {
            return None;
        }
        let frame = self.rbuf[4..4 + len].to_vec();
        self.rbuf.drain(..4 + len);
        Some(frame)
    }
}

/// Frame helpers for the other side of a socket — what a test's fake peer
/// (or a broker) needs. Kept here so the agent's own framing is what the
/// tests exercise.
pub fn read_frame(s: &mut UnixStream) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    s.read_exact(&mut len).ok()?;
    let len = u32::from_le_bytes(len) as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).ok()?;
    Some(buf)
}

pub fn write_frame(s: &mut UnixStream, bytes: &[u8]) -> std::io::Result<()> {
    s.write_all(&(bytes.len() as u32).to_le_bytes())?;
    s.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn frames_round_trip_through_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let frame = read_frame(&mut s).expect("a frame");
            let req: CollabRequest = serde_json::from_slice(&frame).unwrap();
            assert!(matches!(req, CollabRequest::Hello { ref app_id, .. } if app_id == "dev.qos.word-processor"));
            let ev = serde_json::to_vec(&CollabEvent::CRDTIncremental { data: vec![1, 2, 3] }).unwrap();
            // Split the frame across two writes: the reader must reassemble.
            let len = (ev.len() as u32).to_le_bytes();
            s.write_all(&len).unwrap();
            s.write_all(&ev[..2]).unwrap();
            std::thread::sleep(Duration::from_millis(30));
            s.write_all(&ev[2..]).unwrap();
        });
        let mut link = Link::connect(&path).unwrap();
        link.send(&CollabRequest::Hello {
            app_id: "dev.qos.word-processor".into(),
            cluster_id: uuid::Uuid::new_v4(),
            record: None,
            handle: Some("Scribe".into()),
        })
        .unwrap();
        let mut got = None;
        for _ in 0..50 {
            if let Some(ev) = link.recv(Duration::from_millis(20)).unwrap() {
                got = Some(ev);
                break;
            }
        }
        assert!(matches!(got, Some(CollabEvent::CRDTIncremental { ref data }) if data == &[1, 2, 3]));
        server.join().unwrap();
    }

    // Replacing one descriptor with another, which is what an unrelated
    // close of a number this process no longer owns leaves behind once
    // something else has taken it.
    extern "C" {
        #[link_name = "dup2"]
        fn libc_dup2(oldfd: i32, newfd: i32) -> i32;
    }

    /// A read of zero on a descriptor that is still the connection is the
    /// far end hanging up, and must keep saying so in the words the
    /// harnesses grep for.
    #[test]
    fn an_ordinary_hang_up_still_reads_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            drop(s); // hang up, for real
        });
        let mut link = Link::connect(&path).unwrap();
        let err = loop {
            match link.recv(Duration::from_millis(200)) {
                Err(e) => break e.to_string(),
                Ok(_) => continue,
            }
        };
        assert_eq!(
            err, "the broker closed the connection",
            "a real hang-up has to keep its original wording",
        );
        server.join().unwrap();
    }

    /// And a read of zero on a descriptor that is *not* the connection any
    /// more says that instead — because the two are the same `Ok(0)` and
    /// opposite diagnoses.
    ///
    /// Forced rather than waited for: the descriptor is replaced outright
    /// with another socket whose far end has gone, which is what reading
    /// zero from a number somebody else took looks like from in here. The
    /// replacement is a socket on purpose — in the failure this was built
    /// for, `set_read_timeout` succeeded just before the zero-read, so
    /// whatever the descriptor was by then was still a socket.
    #[test]
    fn a_descriptor_that_is_no_longer_the_connection_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            // Hold the real connection open for the length of the test, so
            // that a zero-read cannot possibly be this end hanging up.
            std::thread::sleep(Duration::from_millis(400));
            drop(s);
        });
        let mut link = Link::connect(&path).unwrap();
        let opened_on = link
            .opened_on
            .clone()
            .expect("the link records what it opened on");

        // Another socket, already at end of stream, put on the link's number.
        let (spare, gone) = UnixStream::pair().unwrap();
        drop(gone);
        let rc = unsafe { libc_dup2(spare.as_raw_fd(), link.stream.as_raw_fd()) };
        assert_ne!(rc, -1, "replacing the descriptor is the whole test");

        let err = link
            .recv(Duration::from_millis(200))
            .expect_err("a replaced descriptor at end of stream must not read as data")
            .to_string();

        assert!(
            err.contains("no longer the socket it was opened on"),
            "the agent has to say the descriptor changed rather than blame the \
             broker, got {err}",
        );
        assert!(
            err.contains(&opened_on),
            "and it has to say what the descriptor used to be, got {err}",
        );
        assert!(
            !err.starts_with("the broker closed the connection"),
            "and it must not report this as the broker hanging up, got {err}",
        );

        server.join().unwrap();
    }

    #[test]
    fn the_default_socket_follows_the_runtime_dir() {
        // Pure function of the environment; pin the shape the compositor binds.
        let p = default_socket_path();
        assert!(p.ends_with("qos/services/collab-json.sock") || std::env::var_os("QUICK_COLLAB_SOCK").is_some());
    }
}
