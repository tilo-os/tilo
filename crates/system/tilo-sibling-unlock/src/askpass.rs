//! Answering systemd-cryptsetup's passphrase request, as a password agent.
//!
//! The passphrase prompt is up from the start, and the person at the keyboard
//! can type the passphrase or recovery key while a sibling is being asked.
//! Whichever answers first opens the volume. That is the systemd password
//! agent protocol (<https://systemd.io/PASSWORD_AGENTS/>): a request is an
//! `ask.*` file in [`ASK_DIR`], and an agent answers it with one datagram,
//! `+` followed by the passphrase, to the request's socket. The socket checks
//! that the sender is root.
//!
//! Only cryptsetup's request for this volume is answered, and only with a
//! passphrase already checked against the slot, so a wrong answer can never
//! use up one of the person's tries or show them a rejection.

use anyhow::{Context, Result, bail};
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// Where systemd's password requests appear.
pub const ASK_DIR: &str = "/run/systemd/ask-password";

/// One parsed `ask.*` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub id: String,
    pub socket: PathBuf,
    pub pid: Option<u32>,
    /// `CLOCK_MONOTONIC` deadline in microseconds; 0 when there is none.
    pub not_after: u64,
}

/// Parses the `[Ask]` section of a request file.
///
/// # Errors
/// When the file has no socket to answer.
pub fn parse(text: &str) -> Result<Ask> {
    let mut in_ask = false;
    let (mut id, mut socket, mut pid, mut not_after) = (String::new(), None, None, 0);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_ask = line == "[Ask]";
            continue;
        }
        if !in_ask {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Id" => value.trim().clone_into(&mut id),
            "Socket" => socket = Some(PathBuf::from(value.trim())),
            "PID" => pid = value.trim().parse().ok(),
            "NotAfter" => not_after = value.trim().parse().unwrap_or(0),
            _ => {}
        }
    }
    let Some(socket) = socket else {
        bail!("the password request names no socket")
    };
    Ok(Ask {
        id,
        socket,
        pid,
        not_after,
    })
}

/// Whether a request is cryptsetup's, for the volume on `device`.
///
/// systemd-cryptsetup names its request `cryptsetup:<source device>`; the
/// source may be spelt as any link to the same block device, so both sides
/// are resolved before they are compared.
pub fn is_for(ask: &Ask, device: &Path) -> bool {
    let Some(source) = ask.id.strip_prefix("cryptsetup:") else {
        return false;
    };
    let resolve = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    resolve(Path::new(source)) == resolve(device)
}

fn monotonic_micros() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid, writable timespec for the duration of the call.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) };
    if rc != 0 {
        return 0;
    }
    u64::try_from(now.tv_sec)
        .unwrap_or(0)
        .saturating_mul(1_000_000)
        .saturating_add(u64::try_from(now.tv_nsec).unwrap_or(0) / 1000)
}

fn live(ask: &Ask) -> bool {
    if ask.not_after != 0 && monotonic_micros() > ask.not_after {
        return false;
    }
    ask.pid
        .is_none_or(|pid| Path::new(&format!("/proc/{pid}")).exists())
}

/// The pending requests for `device` in `dir`, oldest name first.
pub fn pending(dir: &Path, device: &Path) -> Vec<Ask> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("ask."))
        })
        .collect();
    names.sort();
    names
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .filter_map(|text| parse(&text).ok())
        .filter(|ask| is_for(ask, device) && live(ask))
        .collect()
}

/// Sends `passphrase` as the answer to `ask`.
///
/// # Errors
/// When the request's socket is gone or refuses the datagram.
pub fn answer(ask: &Ask, passphrase: &str) -> Result<()> {
    let mut reply = Zeroizing::new(Vec::with_capacity(passphrase.len().saturating_add(1)));
    reply.push(b'+');
    reply.extend_from_slice(passphrase.as_bytes());
    let socket = UnixDatagram::unbound().context("no datagram socket")?;
    socket
        .send_to(&reply, &ask.socket)
        .with_context(|| format!("could not answer {}", ask.socket.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ASK: &str = "[Ask]\nPID=1\nSocket=/run/systemd/ask-password/sck.1234\nAcceptCached=0\nEcho=0\nNotAfter=0\nMessage=Please enter passphrase for disk root\nId=cryptsetup:/dev/vda3\n";

    #[test]
    fn parses_a_cryptsetup_request() {
        let ask = parse(ASK).unwrap();
        assert_eq!(ask.id, "cryptsetup:/dev/vda3");
        assert_eq!(ask.socket, Path::new("/run/systemd/ask-password/sck.1234"));
        assert_eq!(ask.pid, Some(1));
        assert!(is_for(&ask, Path::new("/dev/vda3")));
        assert!(!is_for(&ask, Path::new("/dev/vda2")));
    }

    #[test]
    fn other_requests_are_left_alone() {
        let pin = ASK.replace("Id=cryptsetup:/dev/vda3", "Id=tilo-rollback:root");
        assert!(!is_for(&parse(&pin).unwrap(), Path::new("/dev/vda3")));
        assert!(parse("[Ask]\nId=cryptsetup:/dev/vda3\n").is_err());
    }

    #[test]
    fn answers_only_live_requests_for_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("sck.1");
        let receiver = UnixDatagram::bind(&socket_path).unwrap();
        let ask = ASK.replace(
            "/run/systemd/ask-password/sck.1234",
            socket_path.to_str().unwrap(),
        );
        std::fs::write(dir.path().join("ask.b"), &ask).unwrap();
        std::fs::write(
            dir.path().join("ask.a"),
            ask.replace("NotAfter=0", "NotAfter=1"),
        )
        .unwrap();
        std::fs::write(dir.path().join("other"), &ask).unwrap();

        let found = pending(dir.path(), Path::new("/dev/vda3"));
        assert_eq!(found.len(), 1, "{found:?}");
        answer(&found[0], "abcd").unwrap();
        let mut buffer = [0_u8; 16];
        let n = receiver.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"+abcd");
    }
}
