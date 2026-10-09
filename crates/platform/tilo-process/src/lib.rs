//! The current process, with one API on every target.
//!
//! On Linux and macOS an app runs as an OS process with an id. In the
//! browser it runs inside the page, which has no process of its own; the
//! functions here answer `None` there instead of panicking as
//! `std::process::id` does.
//!
//! This crate is the platform layer's home for process capabilities; child
//! processes, process groups and PTYs (today's `bounded-command`) move here.

#[cfg(feature = "native-editor")]
pub mod editor;
pub use tilo_platform_caps::Availability;
pub mod rendezvous;

/// A capability of this crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feature {
    /// The current process has an OS process id.
    ProcessId,
    /// Processes meet at a [`rendezvous::Rendezvous`] beside a resource.
    Rendezvous,
}

/// Each feature's availability on Linux, macOS and the web, in that order.
pub const CAPABILITIES: &[(Feature, [Availability; 3])] = &[
    (
        Feature::ProcessId,
        [
            Availability::Available,
            Availability::Available,
            Availability::Unsupported("a page has no process id"),
        ],
    ),
    (
        Feature::Rendezvous,
        [
            Availability::Available,
            Availability::Available,
            Availability::Unsupported("a page has no local sockets"),
        ],
    ),
];

/// Whether `feature` works on the running target.
#[must_use]
pub fn availability(feature: Feature) -> Availability {
    match feature {
        Feature::ProcessId => {
            if backend::HAS_PROCESS_ID {
                Availability::Available
            } else {
                Availability::Unsupported("a page has no process id")
            }
        }
        Feature::Rendezvous => {
            if cfg!(unix) {
                Availability::Available
            } else {
                Availability::Unsupported("a page has no local sockets")
            }
        }
    }
}

/// The current process's OS id, or `None` where apps do not run as OS
/// processes (the browser).
#[must_use]
pub fn id() -> Option<u32> {
    backend::id()
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
///
/// # Errors
///
/// The error reading the table.
pub fn probe_process_table() -> std::io::Result<()> {
    table::probe()
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

    #[test]
    fn availability_matches_the_table_for_this_target() {
        let column = if cfg!(target_family = "wasm") {
            2
        } else {
            usize::from(cfg!(target_os = "macos"))
        };
        for (feature, row) in CAPABILITIES {
            assert_eq!(availability(*feature), row[column]);
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
