//! Read-only privileged capacity review through the canonical helper binary.
use crate::model::{Blocker, HostFacts};
const ARG: &str = "--tilo-staging-capacity-probe";
const MAX_BYTES: u64 = 64 << 10;

pub fn maybe_run(
    args: &[String],
    probe: impl FnOnce() -> HostFacts,
) -> Result<Option<i32>, String> {
    if args.get(1).map(String::as_str) != Some(ARG) {
        return Ok(None);
    }
    if args.len() != 2 {
        return Err("capacity review accepts no device or path arguments".into());
    }
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid has no arguments or pointer accesses.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err("capacity review requires canonical elevation".into());
        }
        use std::io::Write;
        let bytes = serde_json::to_vec(&probe()).map_err(|_| "capacity review encoding failed")?;
        if bytes.len() > MAX_BYTES as usize {
            return Err("capacity review exceeds protocol bound".into());
        }
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|_| "capacity review response failed")?;
        Ok(Some(0))
    }
    #[cfg(not(target_os = "linux"))]
    let _ = probe;
    #[cfg(not(target_os = "linux"))]
    Err("privileged capacity review is available on Linux only".into())
}

pub fn review() -> HostFacts {
    review_with_cancel(&std::sync::atomic::AtomicBool::new(false))
}

pub fn review_with_cancel(cancel: &std::sync::atomic::AtomicBool) -> HostFacts {
    #[cfg(target_os = "linux")]
    {
        let Ok(executable) = super::process::helper_executable() else {
            return HostFacts::unsupported(Blocker::DiskUnsupported);
        };
        let mut command = std::process::Command::new("pkexec");
        command.arg(executable).arg(ARG);
        review_command(command, cancel)
    }
    #[cfg(not(target_os = "linux"))]
    {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            HostFacts::unsupported(Blocker::DiskUnsupported)
        } else {
            native_probe()
        }
    }
}

#[cfg(target_os = "linux")]
fn review_command(
    command: std::process::Command,
    cancel: &std::sync::atomic::AtomicBool,
) -> HostFacts {
    use std::sync::atomic::Ordering;
    let refused = || HostFacts::unsupported(Blocker::DiskUnsupported);
    if cancel.load(Ordering::Acquire) {
        return refused();
    }
    let Ok(mut owned) = bounded_command::spawn_prepared(
        command,
        None,
        MAX_BYTES as usize,
        std::time::Duration::from_secs(120),
    ) else {
        return refused();
    };
    loop {
        if cancel.load(Ordering::Acquire) {
            // The canonical handle's Drop cancels the exact owned process
            // group and joins its output/retirement worker before returning.
            drop(owned);
            return refused();
        }
        if let Some(result) = owned.try_wait() {
            return match result {
                Ok(output) if output.status.success() && !cancel.load(Ordering::Acquire) => {
                    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| refused())
                }
                _ => refused(),
            };
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn cancellation_retires_the_actual_blocked_capacity_child_before_returning() {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("pid");
        let mut command = std::process::Command::new("/bin/sh");
        command.args([
            "-c",
            "printf '%s' \"$$\" > \"$1\"; exec sleep 30",
            "capacity-fixture",
        ]);
        command.arg(&marker);
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let worker = std::thread::spawn(move || review_command(command, &flag));
        let deadline = tilo_test_env::hang_deadline();
        let pid = loop {
            if let Ok(bytes) = std::fs::read_to_string(&marker)
                && let Ok(pid) = bytes.parse::<libc::pid_t>()
            {
                break pid;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        // SAFETY: pidfd_open observes the actual child announced above and
        // creates no capability or signal; the returned fd is owned at once.
        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        assert!(raw >= 0);
        // SAFETY: this is the freshly owned descriptor from pidfd_open.
        let pidfd = unsafe { std::fs::File::from_raw_fd(raw as libc::c_int) };
        cancel.store(true, Ordering::Release);
        while !worker.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(worker.join().unwrap().basis.is_none());
        let mut event = libc::pollfd {
            fd: pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll observes one initialized owned descriptor.
        assert_eq!(unsafe { libc::poll(&raw mut event, 1, 0) }, 1);
        assert_ne!(event.revents & libc::POLLIN, 0);
    }

    #[test]
    fn capacity_helper_rejects_caller_selected_devices_before_probing() {
        assert!(
            maybe_run(
                &["imager".into(), ARG.into(), "/dev/arbitrary".into()],
                || panic!("must reject before probing")
            )
            .is_err()
        );
        assert_eq!(
            maybe_run(&["imager".into(), "--other".into()], || panic!(
                "must not probe"
            ))
            .unwrap(),
            None
        );
    }
}

#[cfg(not(target_os = "linux"))]
fn native_probe() -> HostFacts {
    #[cfg(windows)]
    {
        super::windows::probe_facts()
    }
    #[cfg(not(windows))]
    {
        HostFacts::unsupported(Blocker::DiskUnsupported)
    }
}
