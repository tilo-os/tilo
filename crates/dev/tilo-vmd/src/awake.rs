//! The keep-awake lease: a logind `sleep` block lock that keeps the machine
//! from suspending while someone drives it with tilo-vm.
//!
//! Every RPC connection renews the lease until [`AWAKE_IDLE_SECONDS`] after
//! it, and `tilo-vm vm awake --for` holds it longer. The lease outlives the
//! connection: it is a file the daemon writes ([`Lease`]) and a transient
//! unit, [`AWAKE_UNIT`], whose `tilo-vmd awake-hold` keeps one
//! `systemd-inhibit --what=sleep --mode=block` in place until the deadline in
//! that file passes. A renewal only rewrites the file; the holder takes a new
//! lock naming the new deadline before it drops the old one, so the machine
//! is never unheld in between. A lost connection does not end the hold, and
//! a client that never comes back does not keep a battery machine awake for
//! longer than the deadline.

use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_vm_protocol::AWAKE_IDLE_SECONDS;

/// The transient unit that holds the lock.
pub const AWAKE_UNIT: &str = "tilo-vm-awake.service";

/// The lease's file under the daemon's state root.
pub const LEASE_FILE: &str = "awake.json";

/// A renewal that moves the deadline by less than this leaves the file as it
/// is: many connections in a row cost one write, not one each.
const RENEW_GRANULARITY: u64 = 60;

/// How often the holder rereads the lease file.
const POLL: Duration = Duration::from_secs(5);

/// When the lease ends, in Unix seconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// [`AWAKE_IDLE_SECONDS`] after the last request.
    pub idle_until: u64,
    /// An explicit hold's end (`tilo-vm vm awake --for`).
    #[serde(default)]
    pub hold_until: Option<u64>,
}

/// What renews the lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Any request: awake until [`AWAKE_IDLE_SECONDS`] from now, at least.
    Request,
    /// Awake this many seconds from now, longer or shorter than a request's.
    Hold(u64),
    /// Release it now.
    Off,
}

impl Lease {
    /// When the machine may suspend again.
    pub fn deadline(&self) -> u64 {
        self.hold_until
            .map_or(self.idle_until, |hold| hold.max(self.idle_until))
    }

    /// Does it hold the machine at `now`?
    pub fn active(&self, now: u64) -> bool {
        self.deadline() > now
    }

    /// The lease after `change` at `now`.
    #[must_use]
    pub fn renewed(self, now: u64, change: Change) -> Self {
        let hold_until = self.hold_until.filter(|&hold| hold > now);
        match change {
            Change::Request => Self {
                idle_until: self.idle_until.max(now.saturating_add(AWAKE_IDLE_SECONDS)),
                hold_until,
            },
            Change::Hold(seconds) => Self {
                idle_until: now,
                hold_until: Some(now.saturating_add(seconds)),
            },
            Change::Off => Self {
                idle_until: now,
                hold_until: None,
            },
        }
    }
}

/// Whether `change` needs the lease file rewritten, and the lease it then
/// holds. A request that would move a running lease's deadline by less than
/// `RENEW_GRANULARITY` needs nothing; an explicit hold or release always
/// writes.
pub fn renewal(stored: Option<Lease>, now: u64, change: Change) -> (Lease, bool) {
    let old = stored.unwrap_or_default();
    let new = old.renewed(now, change);
    let write = match change {
        Change::Request => {
            stored.is_none()
                || !old.active(now)
                || new.deadline().saturating_sub(old.deadline()) >= RENEW_GRANULARITY
        }
        Change::Hold(_) | Change::Off => true,
    };
    (if write { new } else { old }, write)
}

/// The lease in `path`; `None` when there is none or it cannot be read.
pub fn read(path: &Path) -> Option<Lease> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `2026-10-04 14:32 UTC`.
pub fn utc(seconds: u64) -> String {
    i64::try_from(seconds)
        .ok()
        .and_then(|s| chrono::DateTime::from_timestamp(s, 0))
        .map_or_else(
            || format!("{seconds}"),
            |time| time.format("%Y-%m-%d %H:%M UTC").to_string(),
        )
}

/// The `systemd-inhibit` that holds the lock until `deadline`: the why
/// names the deadline, and its own `sleep` ends a little after it in case
/// the holder dies without its unit.
pub fn inhibitor_argv(deadline: u64, now: u64) -> Vec<String> {
    let remaining = deadline.saturating_sub(now).saturating_add(10);
    vec![
        "systemd-inhibit".into(),
        "--what=sleep".into(),
        "--mode=block".into(),
        "--who=tilo-vm".into(),
        format!(
            "--why=someone is working on this machine with tilo-vm; it stays awake until {} \
             ({} minutes after their last request, or tilo-vm vm awake --for)",
            utc(deadline),
            AWAKE_IDLE_SECONDS / 60
        ),
        "sleep".into(),
        remaining.to_string(),
    ]
}

/// One `systemd-inhibit` in its own process group, so stopping it also
/// stops the `sleep` it runs.
struct Inhibitor {
    child: Child,
    deadline: u64,
}

impl Inhibitor {
    fn spawn(deadline: u64, now: u64) -> std::io::Result<Self> {
        use std::os::unix::process::CommandExt as _;
        let line = inhibitor_argv(deadline, now);
        let Some((program, args)) = line.split_first() else {
            return Err(std::io::Error::other("no inhibitor command"));
        };
        let child = Command::new(program).args(args).process_group(0).spawn()?;
        Ok(Self { child, deadline })
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(mut self) {
        if let Some(group) = i32::try_from(self.child.id())
            .ok()
            .and_then(i32::checked_neg)
        {
            // SAFETY: signals the process group this inhibitor leads.
            unsafe {
                libc::kill(group, libc::SIGTERM);
            }
        }
        let _ = self.child.wait();
    }
}

/// `tilo-vmd awake-hold`: keep a lock in place until the lease in `path`
/// ends, taking a new one (before dropping the old) whenever its deadline
/// moves.
///
/// # Errors
/// `systemd-inhibit` could not be started.
pub fn hold(path: &Path) -> std::io::Result<()> {
    let mut current: Option<Inhibitor> = None;
    loop {
        let now = tilo_clock::unix_now();
        let deadline = read(path).map_or(0, |lease| lease.deadline());
        if deadline <= now {
            if let Some(inhibitor) = current.take() {
                inhibitor.stop();
            }
            return Ok(());
        }
        let stale = match current.as_mut() {
            Some(inhibitor) => !inhibitor.running() || inhibitor.deadline != deadline,
            None => true,
        };
        if stale {
            let next = Inhibitor::spawn(deadline, now)?;
            if let Some(old) = current.replace(next) {
                old.stop();
            }
        }
        std::thread::sleep(POLL.min(Duration::from_secs(deadline.saturating_sub(now))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    /// Every request keeps the machine awake 30 minutes past it, an explicit
    /// hold sets the deadline itself, and release ends it at once.
    #[test]
    fn the_lease_lasts_thirty_minutes_past_the_last_request() {
        let first = Lease::default().renewed(NOW, Change::Request);
        assert_eq!(first.deadline(), NOW + 1800);
        assert!(first.active(NOW + 1799));
        assert!(!first.active(NOW + 1800));
        // A later request moves it on; an earlier deadline never shortens it.
        let later = first.renewed(NOW + 600, Change::Request);
        assert_eq!(later.deadline(), NOW + 2400);
        // --for 2h: longer than a request's, and requests inside it keep it.
        let held = later.renewed(NOW + 600, Change::Hold(7200));
        assert_eq!(held.deadline(), NOW + 7800);
        assert_eq!(
            held.renewed(NOW + 700, Change::Request).deadline(),
            NOW + 7800
        );
        // A shorter hold is what was asked for, until the next request.
        let short = later.renewed(NOW + 600, Change::Hold(60));
        assert_eq!(short.deadline(), NOW + 660);
        let after = held.renewed(NOW + 8000, Change::Request);
        assert_eq!(after.hold_until, None);
        assert_eq!(after.deadline(), NOW + 9800);
        let off = held.renewed(NOW + 700, Change::Off);
        assert!(!off.active(NOW + 700));
        assert_eq!(off.hold_until, None);
    }

    /// A burst of connections writes the lease once; a hold, a release and a
    /// lapsed lease always write.
    #[test]
    fn renewing_writes_only_when_the_deadline_moves() {
        let (lease, write) = renewal(None, NOW, Change::Request);
        assert!(write);
        assert_eq!(lease.deadline(), NOW + 1800);
        // Seconds later: nothing to write, the stored lease stands.
        let (same, write) = renewal(Some(lease), NOW + 30, Change::Request);
        assert!(!write);
        assert_eq!(same, lease);
        let (moved, write) = renewal(Some(lease), NOW + 60, Change::Request);
        assert!(write);
        assert_eq!(moved.deadline(), NOW + 1860);
        // Lapsed: renewed even within the granularity of the old deadline.
        let lapsed = Lease {
            idle_until: NOW - 10,
            hold_until: None,
        };
        assert!(renewal(Some(lapsed), NOW, Change::Request).1);
        assert!(renewal(Some(lease), NOW, Change::Hold(3600)).1);
        let (off, write) = renewal(Some(lease), NOW, Change::Off);
        assert!(write);
        assert!(!off.active(NOW));
    }

    /// The lock is a sleep-only block lock by tilo-vm that names its deadline.
    #[test]
    fn the_inhibitor_blocks_sleep_and_names_its_deadline() {
        let argv = inhibitor_argv(NOW + 1800, NOW);
        assert_eq!(
            argv[..4],
            [
                "systemd-inhibit",
                "--what=sleep",
                "--mode=block",
                "--who=tilo-vm"
            ]
        );
        assert!(
            argv[4].contains("until 2027-01-15 08:30 UTC"),
            "{}",
            argv[4]
        );
        assert_eq!(argv[5..], ["sleep", "1810"]);
    }
}
