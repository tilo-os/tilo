//! The socket: anyone may connect, and the peer's credentials decide what
//! they may ask.

use super::{Answer, Daemon, Peer};
use crate::protocol::{MAX_LINE, Refusal, Reply, Request};
use anyhow::{Context as _, Result};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use zeroize::Zeroizing;

/// More than every sign-in surface at once; a flood of idle connections
/// cannot exhaust the daemon.
const MAX_CONNECTIONS: usize = 32;
const IDLE: Duration = Duration::from_secs(120);

pub fn bind(daemon: &Daemon) -> Result<UnixListener> {
    let path = &daemon.config().socket;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
    }
    let listener =
        UnixListener::bind(path).with_context(|| format!("listening on {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o666))
        .with_context(|| format!("opening {} to every local process", path.display()))?;
    Ok(listener)
}

pub fn serve(daemon: &Arc<Daemon>, listener: &UnixListener) {
    let open = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        if open.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::SeqCst);
            continue;
        }
        let daemon = Arc::clone(daemon);
        let open = Arc::clone(&open);
        std::thread::spawn(move || {
            if let Err(e) = connection(&daemon, stream) {
                eprintln!("tilo-authd: connection ended: {e:#}");
            }
            open.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

fn peer(stream: &UnixStream) -> Result<Peer> {
    let cred = rustix::net::sockopt::socket_peercred(stream).context("reading the peer")?;
    Ok(Peer {
        uid: cred.uid.as_raw(),
    })
}

fn connection(daemon: &Daemon, stream: UnixStream) -> Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    let peer = peer(&stream)?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = Zeroizing::new(Vec::new());
        let read = (&mut reader)
            .take(u64::try_from(MAX_LINE).unwrap_or(u64::MAX))
            .read_until(b'\n', &mut line)?;
        if read == 0 {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            send(
                &mut writer,
                &Answer::Reply(Reply::refused(Refusal::BadRequest)),
            )?;
            return Ok(());
        }
        let answer = match serde_json::from_slice::<Request>(&line) {
            Ok(request) => daemon.handle(peer, request),
            Err(_) => Answer::Reply(Reply::refused(Refusal::BadRequest)),
        };
        send(&mut writer, &answer)?;
    }
}

fn send(writer: &mut UnixStream, answer: &Answer) -> Result<()> {
    let mut bytes = match answer {
        Answer::Reply(reply) => serde_json::to_vec(reply),
        Answer::Pending(pending) => serde_json::to_vec(pending),
    }
    .context("encoding a reply")?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    Ok(())
}
