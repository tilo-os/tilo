//! The legacy JSON collaboration socket: `u32` little-endian length prefix,
//! then a `serde_json` body — `CollabRequest` outbound, `CollabEvent`
//! inbound. Mirrors `ui::collaboration` (the app side) and
//! `shell::shell::collab_broker` (the broker side) byte for byte.

use std::io::{Read, Write};
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
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(|| {
            let uid = unsafe { libc_getuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        }, PathBuf::from);
    runtime
        .join("qos")
        .join("services")
        .join("collab-json.sock")
}

// Avoid a `libc` dependency for one call.
unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// One connection to the broker.
pub struct Link {
    stream: UnixStream,
    rbuf: Vec<u8>,
}

impl Link {
    pub fn connect(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path).with_context(|| {
            format!("connect to the collaboration broker at {}", path.display())
        })?;
        Ok(Self {
            stream,
            rbuf: Vec::new(),
        })
    }

    /// Write one request. Blocking: a full-document snapshot can be large
    /// and must not be truncated by a would-block.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        let body = serde_json::to_vec(req).context("encode request")?;
        self.stream.set_nonblocking(false)?;
        self.stream
            .write_all(&(body.len() as u32).to_le_bytes())
            .and_then(|()| self.stream.write_all(&body))
            .context("write to the broker")?;
        Ok(())
    }

    /// Read the next event, waiting at most `timeout`. `Ok(None)` on
    /// timeout; `Err` once the broker has gone away.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        loop {
            if let Some(frame) = self.take_frame() {
                return Ok(Some(
                    serde_json::from_slice(&frame).context("decode event")?,
                ));
            }
            self.stream.set_nonblocking(false)?;
            self.stream.set_read_timeout(Some(timeout))?;
            let mut chunk = [0u8; 64 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => anyhow::bail!("the broker closed the connection"),
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

    fn take_frame(&mut self) -> Option<Vec<u8>> {
        if self.rbuf.len() < 4 {
            return None;
        }
        let len =
            u32::from_le_bytes([self.rbuf[0], self.rbuf[1], self.rbuf[2], self.rbuf[3]]) as usize;
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
            assert!(
                matches!(req, CollabRequest::Hello { ref app_id, .. } if app_id == "dev.qos.word-processor")
            );
            let ev = serde_json::to_vec(&CollabEvent::CRDTIncremental {
                data: vec![1, 2, 3],
            })
            .unwrap();
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
        assert!(
            matches!(got, Some(CollabEvent::CRDTIncremental { ref data }) if data == &[1, 2, 3])
        );
        server.join().unwrap();
    }

    #[test]
    fn the_default_socket_follows_the_runtime_dir() {
        // Pure function of the environment; pin the shape the compositor binds.
        let p = default_socket_path();
        assert!(
            p.ends_with("qos/services/collab-json.sock")
                || std::env::var_os("QUICK_COLLAB_SOCK").is_some()
        );
    }
}
