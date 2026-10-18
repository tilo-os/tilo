//! Ending the person's own apps and processes.
//!
//! A pid alone is not a target: it is reused. Every request names the
//! process's start time too, and the process is pinned with a pidfd
//! *before* its identity, owner and cgroup are read again, so the signal can
//! only reach the process that was checked — never one that took its pid
//! in between.

use crate::apps::EndPolicy;
use crate::probe::Probe;
use crate::protocol::Refusal;

/// Something that can deliver a signal to exactly one process.
pub trait Signaller: Send {
    /// Pin `pid`, run `check` against what it is now, and signal it if the
    /// check passes.
    ///
    /// # Errors
    /// The check's refusal, [`Refusal::Gone`] when there is no such process,
    /// or [`Refusal::Failed`] when the kernel refuses.
    fn signal_checked(
        &self,
        pid: u32,
        force: bool,
        check: &mut dyn FnMut() -> Result<(), Refusal>,
    ) -> Result<(), Refusal>;
}

/// Signals through a pidfd (Linux 5.3).
#[derive(Clone, Copy, Debug, Default)]
pub struct PidfdSignaller;

impl Signaller for PidfdSignaller {
    #[cfg(target_os = "linux")]
    fn signal_checked(
        &self,
        pid: u32,
        force: bool,
        check: &mut dyn FnMut() -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        use rustix::process::{Pid, PidfdFlags, Signal, pidfd_open, pidfd_send_signal};
        let raw = i32::try_from(pid).map_err(|_| Refusal::Gone)?;
        let pid = Pid::from_raw(raw).ok_or(Refusal::Gone)?;
        let pidfd = pidfd_open(pid, PidfdFlags::empty()).map_err(|error| {
            if error == rustix::io::Errno::SRCH {
                Refusal::Gone
            } else {
                Refusal::Failed {
                    detail: error.to_string(),
                }
            }
        })?;
        check()?;
        let signal = if force { Signal::KILL } else { Signal::TERM };
        pidfd_send_signal(&pidfd, signal).map_err(|error| {
            if error == rustix::io::Errno::SRCH {
                Refusal::Gone
            } else {
                Refusal::Failed {
                    detail: error.to_string(),
                }
            }
        })
    }

    #[cfg(not(target_os = "linux"))]
    fn signal_checked(
        &self,
        _pid: u32,
        _force: bool,
        _check: &mut dyn FnMut() -> Result<(), Refusal>,
    ) -> Result<(), Refusal> {
        Err(Refusal::Failed {
            detail: "ending processes is served on Linux".into(),
        })
    }
}

/// End the process `pid` of the lifetime `started_at`, if `policy` allows.
///
/// # Errors
/// Why it was not ended.
pub fn end_process(
    probe: &mut dyn Probe,
    policy: &EndPolicy,
    signaller: &dyn Signaller,
    target: (u32, u64),
    force: bool,
) -> Result<(), Refusal> {
    let (pid, started_at) = target;
    signaller.signal_checked(pid, force, &mut || {
        let process = probe.process(pid).ok_or(Refusal::Gone)?;
        if process.started_at != started_at {
            return Err(Refusal::Gone);
        }
        policy.check(
            pid,
            process.uid,
            process.cgroup.as_deref(),
            process.kernel_thread,
        )
    })
}

/// End every process of `app` the person may end. Returns how many were
/// signalled.
///
/// # Errors
/// [`Refusal::Gone`] when the app runs no process; the policy's refusal
/// when it runs some but the person may end none of them.
pub fn end_app(
    probe: &mut dyn Probe,
    policy: &EndPolicy,
    signaller: &dyn Signaller,
    app: &str,
    force: bool,
) -> Result<u32, Refusal> {
    let targets: Vec<(u32, u64)> = probe
        .processes()
        .into_iter()
        .filter(|process| {
            process
                .cgroup
                .as_deref()
                .and_then(crate::apps::app_of_cgroup)
                .as_deref()
                == Some(app)
        })
        .map(|process| (process.pid, process.started_at))
        .collect();
    if targets.is_empty() {
        return Err(Refusal::Gone);
    }
    let mut ended = 0_u32;
    let mut refusal = None;
    for target in targets {
        match end_process(probe, policy, signaller, target, force) {
            Ok(()) => ended = ended.saturating_add(1),
            // A process that exited meanwhile is as good as ended.
            Err(Refusal::Gone) => {}
            Err(other) => refusal = Some(other),
        }
    }
    match (ended, refusal) {
        (0, Some(refusal)) => Err(refusal),
        (0, None) => Err(Refusal::Gone),
        (ended, _) => Ok(ended),
    }
}
