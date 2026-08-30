//! Blocking facade over the typed collaboration client.
//!
//! Production connects through the launcher-inherited `QOS_SVC_COLLAB_FD`
//! capability. The old framed JSON transport remains only behind
//! [`Link::connect_legacy`] so the existing multi-process characterization
//! fixtures can be retired independently from the production migration.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager};

/// One connection to the broker.
pub struct Link {
    backend: Backend,
}

enum Backend {
    Typed {
        manager: CollaborationManager,
        pending: VecDeque<CollabEvent>,
    },
    Legacy {
        stream: UnixStream,
        rbuf: Vec<u8>,
    },
}

fn take_next_polled(
    pending: &mut VecDeque<CollabEvent>,
    polled: Vec<CollabEvent>,
) -> Option<CollabEvent> {
    pending.extend(polled);
    pending.pop_front()
}

impl Link {
    /// Connect to the typed service capability inherited from the launcher.
    pub fn connect(
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self> {
        let manager = CollaborationManager::for_route(app_id, cluster_id, record, handle)
            .context("connect through QOS_SVC_COLLAB_FD")?;
        Ok(Self {
            backend: Backend::Typed {
                manager,
                pending: VecDeque::new(),
            },
        })
    }

    /// Connect to an explicit typed Cap'n Proto socket. This is for headless
    /// diagnostics that run outside the launcher but still exercise the
    /// production protocol.
    pub fn connect_endpoint(
        path: &Path,
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let manager = CollaborationManager::for_route_fd(
            stream.into_raw_fd(),
            app_id,
            cluster_id,
            record,
            handle,
        )
        .context("start typed collaboration client")?;
        Ok(Self {
            backend: Backend::Typed {
                manager,
                pending: VecDeque::new(),
            },
        })
    }

    /// Compatibility transport for the old socket-based integration fixtures.
    /// Production callers must use [`Self::connect`].
    #[doc(hidden)]
    pub fn connect_legacy(path: &Path) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to the legacy test broker at {}", path.display()))?;
        Ok(Self {
            backend: Backend::Legacy {
                stream,
                rbuf: Vec::new(),
            },
        })
    }

    /// Write one request. Blocking: a full-document snapshot can be large
    /// and must not be truncated by a would-block.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        match &mut self.backend {
            Backend::Typed { manager, .. } => manager.send(req.clone()),
            Backend::Legacy { stream, .. } => {
                let body = serde_json::to_vec(req).context("encode request")?;
                stream.set_nonblocking(false)?;
                stream
                    .write_all(&(body.len() as u32).to_le_bytes())
                    .and_then(|()| stream.write_all(&body))
                    .context("write to the legacy test broker")?;
            }
        }
        Ok(())
    }

    /// Read the next event, waiting at most `timeout`. `Ok(None)` on
    /// timeout; `Err` once the broker has gone away.
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        if let Backend::Typed { manager, pending } = &mut self.backend {
            let deadline = std::time::Instant::now() + timeout;
            loop {
                if let Some(event) = take_next_polled(pending, manager.poll()) {
                    return Ok(Some(event));
                }
                let now = std::time::Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
            }
        }

        loop {
            if let Some(frame) = self.take_frame() {
                return Ok(Some(
                    serde_json::from_slice(&frame).context("decode event")?,
                ));
            }
            let Backend::Legacy { stream, .. } = &mut self.backend else {
                unreachable!("typed backend returned above")
            };
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(timeout))?;
            let mut chunk = [0u8; 64 * 1024];
            match stream.read(&mut chunk) {
                Ok(0) => anyhow::bail!("the broker closed the connection"),
                Ok(n) => {
                    let Backend::Legacy { rbuf, .. } = &mut self.backend else {
                        unreachable!("typed backend returned above")
                    };
                    rbuf.extend_from_slice(&chunk[..n]);
                }
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
        let Backend::Legacy { rbuf, .. } = &mut self.backend else {
            return None;
        };
        if rbuf.len() < 4 {
            return None;
        }
        let len = u32::from_le_bytes([rbuf[0], rbuf[1], rbuf[2], rbuf[3]]) as usize;
        if rbuf.len() < 4 + len {
            return None;
        }
        let frame = rbuf[4..4 + len].to_vec();
        rbuf.drain(..4 + len);
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
        let mut link = Link::connect_legacy(&path).unwrap();
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
    fn typed_poll_batches_keep_every_event_in_order() {
        let mut pending = VecDeque::new();
        let first = take_next_polled(
            &mut pending,
            vec![
                CollabEvent::CRDTIncremental { data: vec![1] },
                CollabEvent::CRDTUpdate { data: vec![2] },
                CollabEvent::CRDTIncremental { data: vec![3] },
            ],
        );

        assert!(matches!(
            first,
            Some(CollabEvent::CRDTIncremental { data }) if data == [1]
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::CRDTUpdate { data }) if data == [2]
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::CRDTIncremental { data }) if data == [3]
        ));
        assert!(take_next_polled(&mut pending, Vec::new()).is_none());
    }
}
