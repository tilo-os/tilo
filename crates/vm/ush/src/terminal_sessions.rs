//! Which Terminal window each Host shell belongs to.
//!
//! A Host shell is the supervisor's child, so the process tree leads from a program typed
//! in a tab up to `tilo-ush`, never to the Terminal window the person sees. The desktop
//! needs that link to serve the tab with its Terminal's workspace scope, so the supervisor
//! writes one small record for each Host session under
//! `$XDG_RUNTIME_DIR/tilo/terminal-sessions/<leader pid>`.
//!
//! Records are read, never trusted on their own: one counts only while its leader and
//! requester are the very processes it names (pid and start time). A Host shell could write
//! one itself, which buys it nothing it could not already do as the person, and a sandboxed
//! app cannot reach the directory.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

pub const DIR: &str = "tilo/terminal-sessions";

const MAX_ANCESTORS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub leader_pid: u32,
    pub leader_start: u64,
    pub requester_pid: u32,
    pub requester_start: u64,
    pub session: String,
}

#[must_use]
pub fn dir(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(runtime_dir?);
    runtime_dir.is_absolute().then(|| runtime_dir.join(DIR))
}

/// Record that `leader_pid` is a Host session `requester_pid` asked for,
/// and drop the records of sessions that have ended. A record outliving its
/// session counts for nothing (its leader is no longer the process it
/// names), so it is only tidied here, never relied on to be gone.
pub fn mark(dir: &Path, leader_pid: u32, requester_pid: u32, session: &str) -> io::Result<Session> {
    let gone = || io::Error::new(io::ErrorKind::NotFound, "the process is gone");
    let (_, leader_start) = proc_stat(leader_pid).ok_or_else(gone)?;
    let (_, requester_start) = proc_stat(requester_pid).ok_or_else(gone)?;
    let session = Session {
        leader_pid,
        leader_start,
        requester_pid,
        requester_start,
        session: session.to_owned(),
    };
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let ended = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
                .is_none_or(|pid| read(dir, pid).is_none_or(|old| !is_live(&old)));
            if ended {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    tilo_files::native::atomic::write_creating_parent(
        &dir.join(leader_pid.to_string()),
        &serde_json::to_vec(&session).map_err(io::Error::other)?,
    )?;
    Ok(session)
}

/// The live Host session `pid` runs in: the nearest ancestor (or `pid`
/// itself) with a record whose leader and requester are still the processes
/// it names.
#[must_use]
pub fn session_of(dir: &Path, mut pid: u32) -> Option<Session> {
    for _ in 0..MAX_ANCESTORS {
        let (parent, start) = proc_stat(pid)?;
        if let Some(session) = read(dir, pid)
            && session.leader_start == start
            && proc_stat(session.requester_pid)
                .is_some_and(|(_, started)| started == session.requester_start)
        {
            return Some(session);
        }
        if parent == 0 || parent == pid {
            return None;
        }
        pid = parent;
    }
    None
}

/// Whether `session` still runs: its leader is the process it names.
#[must_use]
pub fn is_live(session: &Session) -> bool {
    proc_stat(session.leader_pid).is_some_and(|(_, start)| start == session.leader_start)
}

fn read(dir: &Path, leader_pid: u32) -> Option<Session> {
    let bytes = std::fs::read(dir.join(leader_pid.to_string())).ok()?;
    let session: Session = serde_json::from_slice(&bytes).ok()?;
    (session.leader_pid == leader_pid).then_some(session)
}

#[must_use]
pub fn proc_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may hold spaces or parentheses: index from
    // the last ')'. Field 3 (state) comes first, so ppid is 1 and starttime 19.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}
