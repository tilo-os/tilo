//! The current process, with one API on every target.
//!
//! On Linux and macOS an app runs as an OS process with an id; in the browser it runs
//! inside the page, which has none, so the functions here answer `None` rather than
//! panicking as `std::process::id` does.

#[cfg(any(target_os = "android", all(test, unix)))]
pub mod android;
pub mod child;
#[cfg(unix)]
pub mod confinement;
#[cfg(unix)]
pub mod descriptor;
#[cfg(feature = "native-editor")]
pub mod editor;
pub mod env;
pub mod environment;
pub mod library;
pub use tilo_platform_caps::{Availability, availability, available};
#[cfg(unix)]
pub mod local_socket;
pub mod login_shell;
#[cfg(unix)]
pub mod pty;
pub mod rendezvous;
pub mod shared_memory;
pub mod tools;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    ProcessId,
    Rendezvous,
}

impl tilo_platform_caps::Feature for Feature {
    /// The feature's availability on Linux, macOS and the web, in that order.
    fn targets(self) -> [Availability; 3] {
        use Availability::{Available, Unsupported};
        match self {
            Self::ProcessId => [
                Available,
                Available,
                Unsupported("a page has no process id"),
            ],
            Self::Rendezvous => [
                Available,
                Available,
                Unsupported("a page has no local sockets"),
            ],
        }
    }

    fn availability(self) -> Availability {
        let here = match self {
            Self::ProcessId => backend::HAS_PROCESS_ID,
            Self::Rendezvous => cfg!(unix),
        };
        if here {
            Availability::Available
        } else {
            self.targets()[2]
        }
    }
}

/// The current process's OS id, or `None` where apps do not run as OS
/// processes (the browser).
#[must_use]
pub fn id() -> Option<u32> {
    backend::id()
}

#[cfg(unix)]
#[must_use]
pub fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(unix)]
#[must_use]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// How many files `pid` has open, where the system says (Linux's `/proc/<pid>/fd`).
/// `None` elsewhere, and when the process has exited or its table is unreadable;
/// never a made-up zero.
#[must_use]
pub fn open_file_count(pid: u32) -> Option<usize> {
    table::open_file_count(pid)
}

/// The kernel's start time for `pid`, the value that tells a live process from another
/// that reused its id. `None` where the system has no process table, or the process is gone:
/// one that exited and was not reaped yet (a zombie) still has its table entry, but does
/// nothing and holds nothing, so it is gone too.
#[must_use]
pub fn start_time(pid: u32) -> Option<u64> {
    table::start_time(pid)
}

/// One line naming the live process `pid`: its id and [`start_time`], as [`identity_names`]
/// reads it back. `None` where the system cannot see the process.
#[must_use]
pub fn identity_record(pid: u32) -> Option<String> {
    Some(format!("{pid} {}\n", start_time(pid)?))
}

/// Whether `record`, as [`identity_record`] wrote it, names the process `pid` that is alive
/// now: a process that reused the id has another start time, so its predecessor's record
/// never names it. Anything but the one record shape is `false`.
#[must_use]
pub fn identity_names(record: &str, pid: u32) -> bool {
    let mut fields = record.split_whitespace();
    let (Some(record_pid), Some(record_start), None) =
        (fields.next(), fields.next(), fields.next())
    else {
        return false;
    };
    record_pid.parse::<u32>() == Ok(pid) && record_start.parse::<u64>().ok() == start_time(pid)
}

/// Whether this process may read the system's process table: the read error on Linux,
/// `Ok` where no separate table exists to fail, so an empty list can be told apart from
/// a denied one.
pub fn probe_process_table() -> std::io::Result<()> {
    table::probe()
}

/// Raise this process's soft open-file limit to the 1024 descriptors a Linux app starts
/// with, as far as the hard limit allows; macOS starts processes at 256, which one app
/// retaining a descriptor per selected file exhausts.
pub fn raise_open_file_limit() {
    limits::raise_open_files();
}

/// Raise this process's soft open-file limit to its hard limit: what a desktop session takes,
/// which holds descriptors for every window it shows (each client's connection, its buffers and
/// its links). At the 1024 a process starts with, a few dozen windows exhaust it, and then no
/// window, service or person's request gets through. The apps it starts are held to their own
/// limit by the launcher's sandbox, never this one.
#[cfg(unix)]
pub fn raise_open_file_limit_to_hard() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit only writes the rlimit it is given.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0
        || limit.rlim_cur >= limit.rlim_max
    {
        return;
    }
    limit.rlim_cur = limit.rlim_max;
    // SAFETY: setrlimit only reads the initialized rlimit it is given. A refusal leaves the
    // previous limit, which still works for fewer windows.
    let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
}

#[cfg(target_os = "macos")]
mod limits {
    const OPEN_FILES: libc::rlim_t = 1024;

    pub fn raise_open_files() {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit only writes the rlimit it is given.
        if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
            return;
        }
        let wanted = OPEN_FILES.min(limit.rlim_max);
        if limit.rlim_cur >= wanted {
            return;
        }
        limit.rlim_cur = wanted;
        // SAFETY: setrlimit only reads the initialized rlimit it is given. A
        // refusal leaves the previous limit, which is still usable.
        let _ = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) };
    }
}

#[cfg(not(target_os = "macos"))]
mod limits {
    pub const fn raise_open_files() {}
}

#[cfg(target_os = "linux")]
mod table {
    pub fn open_file_count(pid: u32) -> Option<usize> {
        std::fs::read_dir(format!("/proc/{pid}/fd"))
            .ok()
            .map(|entries| entries.flatten().count())
    }

    /// Field 22 of `/proc/<pid>/stat`, of a process whose state (field 3) is not zombie or dead.
    /// `comm` is parenthesised and may itself hold spaces or a `)`, so the last closing
    /// parenthesis is what precedes the fields after it.
    pub fn start_time(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
        if matches!(fields.next()?, "Z" | "X") {
            return None;
        }
        fields.nth(18)?.parse().ok()
    }

    pub fn probe() -> std::io::Result<()> {
        std::fs::read_dir("/proc").map(|_| ())
    }
}

#[cfg(not(target_os = "linux"))]
mod table {
    pub const fn open_file_count(_pid: u32) -> Option<usize> {
        None
    }

    pub const fn start_time(_pid: u32) -> Option<u64> {
        None
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the signature is shared with the Linux backend, whose read can fail"
    )]
    pub const fn probe() -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(not(target_family = "wasm"))]
mod backend {
    pub const HAS_PROCESS_ID: bool = true;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the signature is shared with the browser backend, which has no id"
    )]
    pub fn id() -> Option<u32> {
        Some(std::process::id())
    }
}

#[cfg(target_family = "wasm")]
mod backend {
    pub const HAS_PROCESS_ID: bool = false;

    pub fn id() -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_files_are_counted_only_where_the_system_lists_them() {
        let own = id().unwrap_or(0);
        assert_eq!(open_file_count(own).is_some(), cfg!(target_os = "linux"));
        assert_eq!(open_file_count(u32::MAX), None);
        assert!(probe_process_table().is_ok());
    }

    #[test]
    fn a_process_record_names_its_id_and_start_time_and_nothing_else() {
        let own = id().expect("a native process has an id");
        let record = identity_record(own).expect("the system describes this process");
        assert!(identity_names(&record, own), "{record:?}");
        // Another process, whether a live one or an id nothing has, is never this one.
        assert!(!identity_names(&record, own.wrapping_add(1)));
        let start = start_time(own).expect("a start time");
        assert!(!identity_names(&format!("{own} {}\n", start + 1), own));
        for malformed in ["", "\n", "not a record", "1", "1 2 3"] {
            assert!(!identity_names(malformed, own), "{malformed:?}");
        }
    }

    /// A child that exited and was not reaped yet keeps its table entry, but is not alive: it has
    /// no start time, so nothing takes it for the live process it was.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_exited_process_not_yet_reaped_has_no_start_time() {
        use tilo_test_env::SpawnUnderEnvLock as _;
        let mut child = std::process::Command::new("true")
            .spawn_locked()
            .expect("a child starts");
        let pid = child.id();
        // Wait for the exit without reaping it: the child stays a zombie until `wait`.
        let mut info = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
        // SAFETY: waitid writes only the siginfo it is given; WNOWAIT leaves the child unreaped.
        let waited = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                info.as_mut_ptr(),
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(waited, 0, "{}", std::io::Error::last_os_error());
        assert!(
            std::fs::metadata(format!("/proc/{pid}")).is_ok(),
            "the zombie is still in the table"
        );
        assert_eq!(start_time(pid), None);
        child.wait().expect("reaped");
    }
}
