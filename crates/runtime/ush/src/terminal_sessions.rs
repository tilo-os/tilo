//! Which Terminal window each Host shell belongs to.
//!
//! A Host shell is this supervisor's child, not the Terminal's: the kernel's
//! process tree leads from a program typed in a terminal tab up to
//! `tilo-ush`, never to the Terminal window the person sees. The desktop
//! needs that link to serve the tab with its Terminal's workspace scope
//! (`tilo action`, `tilo open` through the agent door), so the supervisor
//! writes one small record for each Host session:
//! `$XDG_RUNTIME_DIR/tilo/terminal-sessions/<leader pid>`, naming the
//! process that asked for the session (a Terminal launch, or the desktop
//! itself for the Terminal tabs it shows in its own process), both
//! processes' start times, and the session's id.
//!
//! The records are read, never trusted on their own. A record only counts
//! while its leader and requester are the very processes it names (pid and
//! start time). The desktop then attributes the requester through its own
//! record of what it launched, or — when the requester is the desktop
//! itself — finds the tab by the session id that tab's Terminal reported. A Host shell runs as the person, so it
//! could write a record itself; that buys it nothing it could not already do
//! as the person, and a sandboxed app cannot reach this directory at all.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The records' directory under `XDG_RUNTIME_DIR`.
pub const DIR: &str = "tilo/terminal-sessions";

/// How far up a process tree a record is looked for.
const MAX_ANCESTORS: usize = 32;

/// One Host session: its shell and the process that asked for it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub leader_pid: u32,
    pub leader_start: u64,
    pub requester_pid: u32,
    pub requester_start: u64,
    /// The session's id, as the supervisor answered the requester: how a
    /// Terminal the desktop shows in its own process names its tab's
    /// session.
    pub session: String,
}

/// The records' directory, or `None` without an absolute runtime directory.
#[must_use]
pub fn dir(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let runtime_dir = PathBuf::from(runtime_dir?);
    runtime_dir.is_absolute().then(|| runtime_dir.join(DIR))
}

/// Record that `leader_pid` is a Host session `requester_pid` asked for,
/// and drop the records of sessions that have ended. A record outliving its
/// session counts for nothing (its leader is no longer the process it
/// names), so it is only tidied here, never relied on to be gone.
///
/// # Errors
/// Either process is gone, or the record cannot be written.
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

/// A process's parent and start time, from `/proc/<pid>/stat`.
#[must_use]
pub fn proc_stat(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may hold spaces or parentheses: index from
    // the last ')'. Field 3 (state) comes first, so ppid is 1 and starttime 19.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_in_a_session_finds_it_and_loses_it_when_it_ends() {
        let dir = tempfile::tempdir().expect("tempdir");
        let records = dir.path().join("terminal-sessions");
        // This test process stands for the Terminal that asked; a child of
        // it for the session's shell, and the shell's own child for a
        // program typed in it.
        let mut shell = std::process::Command::new("sh")
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("shell");
        let leader = shell.id();
        mark(&records, leader, std::process::id(), "s1").expect("mark");
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(shell.stdout.take().expect("stdout")),
            &mut line,
        )
        .expect("the shell names its child");
        let typed: u32 = line.trim().parse().expect("a pid");
        let found = session_of(&records, typed).expect("in the session");
        assert_eq!(found.leader_pid, leader);
        assert_eq!(found.requester_pid, std::process::id());
        assert_eq!(found.session, "s1");
        assert!(is_live(&found));
        assert_eq!(session_of(&records, std::process::id()), None);
        let _ = std::process::Command::new("kill")
            .arg(typed.to_string())
            .status();
        let _ = shell.wait();
        assert!(!is_live(&found));
        assert_eq!(session_of(&records, leader), None);
        mark(&records, std::process::id(), std::process::id(), "s2").expect("mark");
        assert!(!records.join(leader.to_string()).exists());
    }

    #[test]
    fn a_record_naming_another_process_counts_for_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let me = std::process::id();
        let (_, start) = proc_stat(me).expect("own stat");
        let forged = Session {
            leader_pid: me,
            leader_start: start.wrapping_add(1),
            requester_pid: me,
            requester_start: start,
            session: "s1".into(),
        };
        std::fs::write(
            dir.path().join(me.to_string()),
            serde_json::to_vec(&forged).expect("json"),
        )
        .expect("write");
        assert_eq!(session_of(dir.path(), me), None);
    }
}
