//! The current process, with one API on every target.
//!
//! On Linux and macOS an app runs as an OS process with an id. In the
//! browser it runs inside the page, which has no process of its own; the
//! functions here answer `None` there instead of panicking as
//! `std::process::id` does.
//!
//! This crate is the platform layer's home for process capabilities; child
//! processes, process groups and PTYs (today's `bounded-command`) move here.

pub mod child;
#[cfg(feature = "native-editor")]
pub mod editor;
pub mod job;
pub use tilo_platform_caps::{Availability, availability, available};
#[cfg(unix)]
pub mod local_socket;
pub mod rendezvous;
pub mod shared_memory;
pub mod tools;

/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// The current process has an OS process id.
    ProcessId,
    /// Processes meet at a [`rendezvous::Rendezvous`] beside a resource.
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

    /// Whether the feature works on the running target.
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

/// The real user id this process runs as.
#[cfg(unix)]
#[must_use]
pub fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The effective user id this process acts with: the owner it creates files
/// as and is checked against.
#[cfg(unix)]
#[must_use]
pub fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// How many files process `pid` has open, where the system says (Linux's
/// `/proc/<pid>/fd`). `None` elsewhere, and when the process has exited or
/// its table is not readable; never a made-up zero.
#[must_use]
pub fn open_file_count(pid: u32) -> Option<usize> {
    table::open_file_count(pid)
}

/// Whether this process may read the system's process table: `Ok` where no
/// separate table exists to fail, the read error on Linux (`/proc`), so a
/// process list that comes back empty can be told apart from a denied one.
pub fn probe_process_table() -> std::io::Result<()> {
    table::probe()
}

/// Raise this process's soft open-file limit to the 1024 descriptors a Linux
/// app starts with, as far as the hard limit allows. macOS starts processes
/// at 256, which one app retaining a descriptor per selected file exhausts.
/// The limit is never lowered and the hard limit is left alone; it stops at
/// 1024 so code bounded by `select`'s `FD_SETSIZE` keeps working. Other
/// platforms already start at least that high, so it does nothing there.
pub fn raise_open_file_limit() {
    limits::raise_open_files();
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

    pub fn probe() -> std::io::Result<()> {
        std::fs::read_dir("/proc").map(|_| ())
    }
}

#[cfg(not(target_os = "linux"))]
mod table {
    pub const fn open_file_count(_pid: u32) -> Option<usize> {
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
    use tilo_platform_caps::Feature as _;

    #[test]
    fn availability_matches_the_table_for_this_target() {
        let column = if cfg!(target_family = "wasm") {
            2
        } else {
            usize::from(cfg!(target_os = "macos"))
        };
        for feature in [Feature::ProcessId, Feature::Rendezvous] {
            assert_eq!(availability(feature), feature.targets()[column]);
        }
    }

    #[test]
    fn native_processes_have_an_id() {
        assert_eq!(id().is_some(), cfg!(not(target_family = "wasm")));
    }

    #[test]
    fn open_files_are_counted_only_where_the_system_lists_them() {
        let own = id().unwrap_or(0);
        assert_eq!(open_file_count(own).is_some(), cfg!(target_os = "linux"));
        assert_eq!(open_file_count(u32::MAX), None);
        assert!(probe_process_table().is_ok());
    }
}
