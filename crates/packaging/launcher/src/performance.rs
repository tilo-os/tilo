//! Bounded process-tree performance hints shared by native and compatibility
//! game launch paths.

const SCHED_FLAG_UTIL_CLAMP_MIN: u64 = 0x20;
const IOPRIO_WHO_PROCESS: libc::c_int = 1;
const IOPRIO_CLASS_SHIFT: libc::c_int = 13;
const IOPRIO_CLASS_BEST_EFFORT: libc::c_int = 2;
/// 25% of the kernel's 0..=1024 utilization scale. This reaches a useful
/// bucket with Tilo's five-bucket uclamp configuration, avoiding cold frequency
/// ramps for bursty render/input threads without pinning CPUs near maximum.
pub const GAME_UTIL_MIN: u32 = 256;
/// One microsecond. Narrow wake-up coalescing only for signed game launches to
/// reduce frame-pacing jitter without changing the system-wide timer policy.
pub const GAME_TIMER_SLACK_NS: libc::c_ulong = 1_000;
/// Priority 2 within the ordinary best-effort I/O class. This improves asset
/// and shader-cache reads over default priority 4 without granting realtime
/// I/O or allowing a game to starve kernel writeback indefinitely.
pub const GAME_IO_PRIORITY: libc::c_int = 2;

#[repr(C)]
struct SchedAttr {
    size: u32,
    policy: u32,
    flags: u64,
    nice: i32,
    priority: u32,
    runtime: u64,
    deadline: u64,
    period: u64,
    util_min: u32,
    util_max: u32,
}

/// Apply a best-effort utilization floor to the current process.
///
/// Children inherit the hint. Failure is deliberately ignored: kernels or
/// cgroups without uclamp support must still be able to launch the game.
fn game_sched_attr() -> SchedAttr {
    SchedAttr {
        size: u32::try_from(std::mem::size_of::<SchedAttr>()).unwrap_or(0),
        policy: 0,
        flags: SCHED_FLAG_UTIL_CLAMP_MIN,
        nice: 0,
        priority: 0,
        runtime: 0,
        deadline: 0,
        period: 0,
        util_min: GAME_UTIL_MIN,
        // The kernel validates min <= max even though only the MIN flag is
        // requested. Zero made the otherwise-correct hint fail with EINVAL.
        util_max: 1024,
    }
}

#[cfg(target_os = "linux")]
fn apply_game_uclamp() -> libc::c_long {
    let mut attr = game_sched_attr();
    // Preserve the launcher/cgroup's existing policy, nice value, deadline
    // parameters and utilization ceiling. Supplying zero-filled attributes to
    // sched_setattr would silently reset a deliberately deprioritized game.
    // SAFETY: `attr` is writable for its declared size; pid/flags zero request
    // the calling thread's current scheduler attributes.
    let result = unsafe {
        libc::syscall(
            libc::SYS_sched_getattr,
            0,
            &raw mut attr,
            std::mem::size_of::<SchedAttr>(),
            0,
        )
    };
    if result != 0 {
        return result;
    }
    attr.flags |= SCHED_FLAG_UTIL_CLAMP_MIN;
    attr.util_min = GAME_UTIL_MIN;
    attr.util_max = attr.util_max.max(GAME_UTIL_MIN);
    // SAFETY: `attr` has Linux's sched_attr C layout and remains live for the
    // direct syscall. pid 0 means only the calling process.
    unsafe { libc::syscall(libc::SYS_sched_setattr, 0, &raw const attr, 0) }
}

#[cfg(target_os = "linux")]
fn apply_game_timer_slack() -> libc::c_int {
    // SAFETY: PR_SET_TIMERSLACK consumes one integer value and no pointers.
    // Timer slack, like the utilization clamp, is inherited by game children.
    unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, GAME_TIMER_SLACK_NS, 0, 0, 0) }
}

fn game_ioprio() -> libc::c_int {
    (IOPRIO_CLASS_BEST_EFFORT << IOPRIO_CLASS_SHIFT) | GAME_IO_PRIORITY
}

#[cfg(target_os = "linux")]
fn apply_game_io_priority() -> libc::c_long {
    // SAFETY: ioprio_set consumes scalar values only. WHO_PROCESS/who=0 scopes
    // the best-effort priority to the current process; children inherit it.
    unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, 0, game_ioprio()) }
}

/// Apply the bounded game scheduling profile to this process and its children.
///
/// # Errors
///
/// Returns an OS error when Tilo's required scheduler ABI or timer-slack policy
/// is present but cannot be applied. A kernel with no uclamp support at all is
/// tolerated so packages remain launchable on non-Tilo development hosts.
#[cfg(target_os = "linux")]
pub fn apply_game_performance_hints() -> std::io::Result<()> {
    if apply_game_uclamp() != 0 {
        let error = std::io::Error::last_os_error();
        // Permit launch on a non-Tilo kernel that lacks uclamp entirely. The
        // Tilo kernel pins support, so permission and ABI failures are policy
        // regressions and must not silently discard the gaming profile.
        if error.raw_os_error() != Some(libc::EOPNOTSUPP) {
            return Err(error);
        }
    }
    if apply_game_timer_slack() != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if apply_game_io_priority() != 0 {
        let error = std::io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EOPNOTSUPP)) {
            return Err(error);
        }
    }
    Ok(())
}

/// Non-Linux hosts retain their native scheduler policy. The Linux game hints
/// are optional platform tuning and do not gate launching a game elsewhere.
#[cfg(not(target_os = "linux"))]
pub fn apply_game_performance_hints() -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXEC_PROBE_ENV: &str = "TILO_GAME_HINT_EXEC_PROBE";

    #[cfg(target_os = "linux")]
    fn current_uclamp_min() -> Option<u32> {
        let mut scheduler = game_sched_attr();
        // SAFETY: scheduler is a live writable sched_attr and the remaining
        // scalar arguments request attributes for the calling thread.
        let result = unsafe {
            libc::syscall(
                libc::SYS_sched_getattr,
                0,
                &raw mut scheduler,
                std::mem::size_of::<SchedAttr>(),
                0,
            )
        };
        (result == 0).then_some(scheduler.util_min)
    }

    #[test]
    fn game_hint_is_bounded_and_has_no_realtime_policy() {
        let attr = game_sched_attr();
        assert_eq!(attr.size as usize, std::mem::size_of::<SchedAttr>());
        assert_eq!(attr.policy, 0);
        assert_eq!(attr.priority, 0);
        assert_eq!(attr.util_min, 256);
        assert_eq!(attr.util_max, 1024);
        assert_eq!(attr.util_min, 1024 / 4);
        assert_eq!(GAME_TIMER_SLACK_NS, 1_000);
        assert_eq!(GAME_IO_PRIORITY, 2);
        assert_eq!(game_ioprio() >> IOPRIO_CLASS_SHIFT, 2);
        assert_eq!(game_ioprio() & 7, 2);
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn kernel_applies_scoped_game_hints_through_exec() {
        // Phase `game_hints_survive_exec_into_the_final_process`.
        fn game_hints_survive_exec_into_the_final_process() {
            if std::env::var_os(EXEC_PROBE_ENV).is_some() {
                // This is the post-exec probe. It deliberately reads kernel state
                // rather than trusting values handed to it by the parent.
                // SAFETY: PR_GET_TIMERSLACK has no pointer arguments.
                let timer_slack = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
                assert_eq!(
                    i64::from(timer_slack),
                    i64::try_from(GAME_TIMER_SLACK_NS).unwrap()
                );

                // SAFETY: ioprio_get consumes scalar arguments only.
                let io_priority =
                    unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0) };
                if std::env::var_os("TILO_GAME_HINT_EXPECT_IOPRIO").is_some() {
                    assert_eq!(io_priority, libc::c_long::from(game_ioprio()));
                }
                if std::env::var_os("TILO_GAME_HINT_EXPECT_UCLAMP").is_some() {
                    assert_eq!(current_uclamp_min(), Some(GAME_UTIL_MIN));
                }
                return;
            }

            std::thread::spawn(|| {
                let uclamp_applied = apply_game_uclamp() == 0;
                assert_eq!(apply_game_timer_slack(), 0);
                let ioprio_applied = apply_game_io_priority() == 0;

                let executable = std::env::current_exe().expect("current test executable");
                let mut child = std::process::Command::new(executable);
                child
                    .arg("--exact")
                    .arg("performance::tests::kernel_applies_scoped_game_hints_through_exec")
                    .arg("--nocapture")
                    .env(EXEC_PROBE_ENV, "1");
                if uclamp_applied {
                    child.env("TILO_GAME_HINT_EXPECT_UCLAMP", "1");
                }
                if ioprio_applied {
                    child.env("TILO_GAME_HINT_EXPECT_IOPRIO", "1");
                }
                let output = child.output().expect("execute game-hint probe");
                let stdout = String::from_utf8_lossy(&output.stdout);
                assert!(
                    output.status.success(),
                    "{stdout}\n{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                // libtest exits 0 when `--exact` selects nothing; require the probe ran.
                assert!(
                    stdout.contains("test result: ok. 1 passed"),
                    "the post-exec probe ran no test: {stdout}"
                );
            })
            .join()
            .expect("game exec-inheritance test thread");
        }
        // Child role: the post-exec probe re-enters this test and must read the
        // inherited kernel state before any other phase runs.
        if std::env::var_os(EXEC_PROBE_ENV).is_some() {
            game_hints_survive_exec_into_the_final_process();
            return;
        }
        {
            std::thread::spawn(|| {
                let applied = apply_game_uclamp();
                let uclamp_error = (applied != 0).then(std::io::Error::last_os_error);
                assert_eq!(apply_game_timer_slack(), 0);
                if applied == 0 {
                    let mut scheduler = game_sched_attr();
                    // SAFETY: scheduler points to a live sched_attr-sized writable
                    // object; pid/flags zero request the calling thread.
                    let result = unsafe {
                        libc::syscall(
                            libc::SYS_sched_getattr,
                            0,
                            &raw mut scheduler,
                            std::mem::size_of::<SchedAttr>(),
                            0,
                        )
                    };
                    assert_eq!(result, 0);
                    assert_eq!(scheduler.util_min, GAME_UTIL_MIN);
                } else {
                    assert_eq!(
                        uclamp_error.and_then(|error| error.raw_os_error()),
                        Some(libc::EOPNOTSUPP),
                        "uclamp may be absent, but permission/ABI errors are regressions"
                    );
                }
                // SAFETY: PR_GET_TIMERSLACK takes no pointer arguments and returns
                // the current calling thread's value directly.
                let actual = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
                assert_eq!(actual, 1_000);
                let io_applied = apply_game_io_priority();
                let io_error = (io_applied != 0).then(std::io::Error::last_os_error);
                if io_applied == 0 {
                    // SAFETY: ioprio_get consumes scalar arguments and returns the
                    // packed class/priority for the current process.
                    let actual =
                        unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, 0) };
                    assert_eq!(actual, libc::c_long::from(game_ioprio()));
                } else {
                    assert!(matches!(
                        io_error.and_then(|error| error.raw_os_error()),
                        Some(libc::ENOSYS | libc::EOPNOTSUPP)
                    ));
                }
            })
            .join()
            .expect("game-hint test thread");
        }
        {
            std::thread::spawn(|| {
                apply_game_performance_hints().expect("Tilo game scheduling profile");
                // SAFETY: PR_GET_TIMERSLACK returns the calling thread's scalar
                // value and consumes no pointer arguments.
                let actual = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
                assert_eq!(actual, 1_000);
            })
            .join()
            .expect("game profile test thread");
        }
        {
            std::thread::spawn(|| {
                // SAFETY: setpriority consumes scalar arguments. A positive nice
                // value is always an unprivileged, thread-local de-prioritization
                // on Linux and keeps this mutation inside the test thread.
                assert_eq!(unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 7) }, 0);
                let before = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
                assert_eq!(before, 7);

                let applied = apply_game_uclamp();
                if applied == 0 {
                    let after = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
                    assert_eq!(after, before);
                } else {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::EOPNOTSUPP)
                    );
                }
            })
            .join()
            .expect("nice-preservation test thread");
        }
        // Phase `game_hints_survive_exec_into_the_final_process` (defined above).
        game_hints_survive_exec_into_the_final_process();
    }
}
