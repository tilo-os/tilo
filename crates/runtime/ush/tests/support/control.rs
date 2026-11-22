//! The supervisor's control connection as Terminal opens it: the launcher's
//! handoff, then the hello, then requests; a session arrives as its pty and
//! its own lease.

use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;

use ush::supervisor::ServeResponse;

/// Connect to `socket`, send `handoff` (the launcher's, when there is one)
/// and the hello, and read the supervisor's ready.
pub fn connect(socket: &Path, handoff: Option<&[u8]>) -> UnixStream {
    let stream = UnixStream::connect(socket).expect("connect");
    if let Some(handoff) = handoff {
        (&stream).write_all(handoff).expect("write handoff");
    }
    (&stream)
        .write_all(b"{\"op\":\"hello\",\"protocol\":2}\n")
        .expect("write hello");
    let mut ready = Vec::new();
    let mut byte = [0_u8; 1];
    while ready.last() != Some(&b'\n') {
        assert_eq!(
            (&stream).read(&mut byte).expect("read ready"),
            1,
            "no ready"
        );
        ready.push(byte[0]);
    }
    let ready = String::from_utf8_lossy(&ready);
    assert!(ready.contains("\"ready\""), "{ready}");
    stream
}

/// A session the supervisor handed out: its pty and the lease that holds it.
pub struct Session {
    pub master: OwnedFd,
    pub lease: UnixStream,
}

/// Write `request` and read its answer, with the session it opened.
pub fn ask(stream: &UnixStream, request: &serde_json::Value) -> (ServeResponse, Option<Session>) {
    let mut line = serde_json::to_vec(request).expect("encode request");
    line.push(b'\n');
    (&*stream).write_all(&line).expect("write request");
    let (payload, rights) = runtime_portal::fd_transport::recv_with_fds(stream).expect("recvmsg");
    let text = String::from_utf8_lossy(&payload);
    let response: ServeResponse =
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("bad reply {text:?}: {e}"));
    let mut rights = rights.into_iter();
    let session = rights.next().map(|master| Session {
        master,
        lease: UnixStream::from(rights.next().expect("a session comes with its lease")),
    });
    (response, session)
}
