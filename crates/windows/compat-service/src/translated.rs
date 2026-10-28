//! Lifetime management for a translated command inside an existing capsule.
//!
//! This does not create or relax a sandbox. The caller must already have
//! entered the capsule, including its namespace and Landlock restrictions.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::dxvk::WineMode;

pub(crate) const ROOTFS: &str = "/opt/tilo/x86";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const PREPARATION_TIMEOUT: Duration = Duration::from_secs(120);

/// Run a guest command while retaining a ready, monitored FEX server.
pub fn run_translated_command(arguments: &[OsString]) -> io::Result<i32> {
    let Some((mode, arguments)) = arguments.split_first() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing guest command",
        ));
    };
    if arguments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing Windows executable",
        ));
    }
    let (mut guest, wine) = match mode.to_str() {
        Some("wine32") => (
            vec![OsString::from("/usr/bin/wine32")],
            Some(WineRuntime {
                mode: WineMode::Win32,
                loader: "/usr/bin/wine32",
                server: "/usr/bin/wineserver32",
            }),
        ),
        Some("wine64") => (
            vec![OsString::from("/usr/bin/wine64")],
            Some(WineRuntime {
                mode: WineMode::Win64,
                loader: "/usr/bin/wine64",
                server: "/usr/bin/wineserver64",
            }),
        ),
        Some("proton") => (
            vec![
                OsString::from("/usr/bin/python3"),
                OsString::from("/usr/libexec/umu_run.py"),
            ],
            None,
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unknown translated runtime",
            ));
        }
    };
    guest.extend_from_slice(arguments);
    let (read_pipe, write_pipe) = readiness_pipe()?;
    let write_fd = write_pipe.as_raw_fd();
    let mut server_command = Command::new("/usr/bin/FEXServer");
    server_command
        .args(["--foreground", "--persistent", "300", "--wait_pipe"])
        .arg(write_fd.to_string())
        .stdin(Stdio::null());
    configure_environment(&mut server_command);
    // SAFETY: fcntl is async-signal-safe. The owned write descriptor remains
    // live until spawn finishes; only this descriptor needs to survive exec.
    unsafe {
        server_command.pre_exec(move || {
            if libc::fcntl(write_fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    // Declaration order matters: on every return path the server is reaped
    // before the initialized prefix lease releases its shared directory lock.
    #[allow(
        clippy::collection_is_never_read,
        reason = "the optional RAII lock intentionally lives until after server destruction"
    )]
    let _prefix_lease;
    let mut server = ManagedChild::new(server_command.spawn()?, false);
    drop(write_pipe);
    wait_for_ready(&read_pipe, &mut server.child, STARTUP_TIMEOUT)?;
    drop(read_pipe);

    // Retain the shared lease through the final Wine server wait. Other
    // initialized launches remain concurrent; upgrades cannot replace their DLLs.
    _prefix_lease = wine
        .as_ref()
        .map(|wine| wine.prepare(&mut server))
        .transpose()?;
    let environment = wine.as_ref().map_or(GuestEnvironment::Unchanged, |wine| {
        GuestEnvironment::WineApplication(wine.mode)
    });
    let status = run_guest(&guest, &mut server, environment, None)?;
    if let Some(wine) = wine {
        wine.wait(&mut server, None)?;
    }
    Ok(status)
}

struct WineRuntime {
    mode: WineMode,
    loader: &'static str,
    server: &'static str,
}

impl WineRuntime {
    fn prepare(&self, server: &mut ManagedChild) -> io::Result<crate::dxvk::PrefixLease> {
        crate::dxvk::prepare(self.mode, |action| {
            // This scope is created only under the exclusive prefix lock.
            // Reaping its descendants precedes returning (and unlocking), even
            // if Wineboot's leader has exited but its detached server has not.
            let descendants = PreparationChildren::new()?;
            let result = (|| {
                let status = run_guest(
                    &[
                        OsString::from(self.loader),
                        // An absolute Unix PE path requires a DOS drive mapping.
                        // The capsule deliberately has no Z: drive. Select Wine's
                        // immutable builtin by name and force builtin-only loading.
                        OsString::from("wineboot.exe"),
                        OsString::from(action.wineboot_argument()),
                    ],
                    server,
                    GuestEnvironment::WineBoot(self.mode),
                    Some(PREPARATION_TIMEOUT),
                )?;
                if status != 0 {
                    return Err(io::Error::other(format!(
                        "Wine prefix update failed: {status}"
                    )));
                }
                self.wait(server, Some(PREPARATION_TIMEOUT))
            })();
            if result.is_err() {
                // Graceful prefix-specific shutdown first. Even a dead FEX
                // server or a failed cleanup command cannot bypass the native
                // descendant cleanup below. Shared launches never reach here.
                let _ = run_guest(
                    &[OsString::from(self.server), OsString::from("-k")],
                    server,
                    GuestEnvironment::WinePreparation(self.mode),
                    Some(Duration::from_secs(10)),
                );
                let _ = self.wait(server, Some(Duration::from_secs(10)));
            }
            drop(descendants);
            result
        })
    }

    fn wait(&self, server: &mut ManagedChild, timeout: Option<Duration>) -> io::Result<()> {
        let status = run_guest(
            &[OsString::from(self.server), OsString::from("-w")],
            server,
            GuestEnvironment::WinePreparation(self.mode),
            timeout,
        )?;
        if status != 0 {
            return Err(io::Error::other(format!(
                "Wine server wait failed: {status}"
            )));
        }
        Ok(())
    }
}

/// Native helper ownership, independent of Wine process groups and FEX's
/// server lifetime. This is deliberately unavailable to multithreaded callers:
/// otherwise another thread could create children inside this ownership scope.
struct PreparationChildren {
    existing: Vec<(libc::pid_t, OwnedFd)>,
    previous_subreaper: libc::c_int,
}

impl PreparationChildren {
    fn new() -> io::Result<Self> {
        if std::fs::read_dir("/proc/self/task")?.count() != 1 {
            return Err(io::Error::other(
                "Wine preparation requires a single-threaded supervisor",
            ));
        }
        Self::acquire()
    }

    fn acquire() -> io::Result<Self> {
        // SIG_IGN and SA_NOCLDWAIT would auto-reap children without an explicit
        // waiter, invalidating our PID ownership between enumeration and open.
        // Custom handlers could do the same; require the helper's default.
        let mut disposition = unsafe { std::mem::zeroed::<libc::sigaction>() };
        // SAFETY: read-only query into a valid sigaction, no process mutation.
        if unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut disposition) } == -1 {
            return Err(io::Error::last_os_error());
        }
        if disposition.sa_sigaction != libc::SIG_DFL
            || disposition.sa_flags & libc::SA_NOCLDWAIT != 0
        {
            return Err(io::Error::other(
                "Wine preparation requires the default waitable SIGCHLD disposition",
            ));
        }
        let existing = direct_children()?
            .into_iter()
            .map(|pid| owned_pidfd(pid).map(|descriptor| (pid, descriptor)))
            .collect::<io::Result<Vec<_>>>()?;
        let mut previous_subreaper = 0;
        // SAFETY: GET writes one live integer; SET takes an integer flag.
        if unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &raw mut previous_subreaper) } == -1 {
            return Err(io::Error::last_os_error());
        }
        // Check pidfd availability and signal permissions before any Wine
        // process starts. The descriptor targets us; signal zero changes nothing.
        let self_pidfd = owned_pidfd(std::process::id().cast_signed())?;
        signal_pidfd(&self_pidfd, 0)?;
        // Also establish that waitid(P_PIDFD) is supported/permitted before
        // spawning. Our own PID is intentionally not a child (ECHILD).
        let _ = still_our_child(&self_pidfd)?;
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            existing,
            previous_subreaper,
        })
    }

    fn reap_descendants(&self) -> io::Result<()> {
        let mut empty_since = None;
        loop {
            let mut children = Vec::new();
            for pid in direct_children()? {
                let mut existing = false;
                for (original_pid, descriptor) in &self.existing {
                    if pid == *original_pid && still_our_child(descriptor)? {
                        existing = true;
                        break;
                    }
                }
                if !existing {
                    children.push(pid);
                }
            }
            if children.is_empty() {
                let since = empty_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_millis(250) {
                    return Ok(());
                }
                // A detached grandchild becomes our child as its direct parent
                // exits. Procfs can briefly publish the empty pre-adoption
                // snapshot even though waitid already observes the new child.
                // Require a quiet interval before releasing the prefix lock.
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            empty_since = None;
            for pid in children {
                // Only our unreaped direct children appear here. No concurrent
                // reaper exists in this single-threaded helper, so the PID
                // cannot be reused before pidfd_open. The pidfd pins identity
                // across signalling; waitpid reaps this exact owned child.
                let descriptor = owned_pidfd(pid)?;
                signal_pidfd(&descriptor, libc::SIGKILL)?;
                loop {
                    // SAFETY: pid is an owned child; no status is requested.
                    if unsafe { libc::waitpid(pid, std::ptr::null_mut(), 0) } != -1 {
                        break;
                    }
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
            // Orphans are synchronously adopted when their parent exits.
            // Repeat for detached grandchildren, including setsid() children.
        }
    }
}

impl Drop for PreparationChildren {
    fn drop(&mut self) {
        // Never unlock a prefix with potentially live initialization writers.
        // Kernel-stuck children or a failed cleanup syscall are exceptional:
        // retain exclusive ownership until cleanup succeeds or the capsule is
        // destroyed. Normal Wine command deadlines remain bounded above.
        let mut reported = false;
        loop {
            match self.reap_descendants() {
                Ok(()) => break,
                Err(error) => {
                    if !reported {
                        eprintln!("Wine descendant cleanup failed; retaining prefix lock: {error}");
                        reported = true;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        // SAFETY: restore the process flag captured before this exclusive scope.
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.previous_subreaper) } == -1 {
            // No descendants remain; restoring this adoption preference cannot
            // compromise the completed prefix cleanup.
            eprintln!(
                "Could not restore child-subreaper state: {}",
                io::Error::last_os_error()
            );
        }
    }
}

fn direct_children() -> io::Result<Vec<libc::pid_t>> {
    let mut children = BTreeSet::new();
    for task in std::fs::read_dir("/proc/self/task")? {
        let task = match task {
            Ok(task) => task,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        let raw = match std::fs::read_to_string(task.path().join("children")) {
            Ok(raw) => raw,
            // A test-harness thread can exit between read_dir and this read.
            // Production preparation is single-threaded, but treating that
            // procfs race as an empty, already-gone task keeps the preflight
            // reliable without weakening child ownership.
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        for pid in raw.split_whitespace() {
            children.insert(
                pid.parse()
                    .map_err(|_| io::Error::other("invalid child PID"))?,
            );
        }
    }

    // `/proc/self/task/*/children` can briefly omit an adopted subreaper
    // child even after waitid(P_PIDFD) reports that we own it. Cross-check
    // process status so cleanup cannot release the prefix lock during that
    // procfs publication race. Entries that disappear or are unreadable are
    // unrelated snapshots; any owned child remains waitable and keeps its PID.
    let our_pid = std::process::id();
    for process in std::fs::read_dir("/proc")? {
        let process = match process {
            Ok(process) => process,
            Err(error) if vanished(&error) => continue,
            Err(error) => return Err(error),
        };
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<libc::pid_t>().ok())
        else {
            continue;
        };
        let status = match std::fs::read_to_string(process.path().join("status")) {
            Ok(status) => status,
            Err(error) if vanished(&error) || error.kind() == io::ErrorKind::PermissionDenied => {
                continue;
            }
            Err(error) => return Err(error),
        };
        let parent = status.lines().find_map(|line| {
            line.strip_prefix("PPid:")
                .and_then(|value| value.trim().parse::<u32>().ok())
        });
        if parent == Some(our_pid) {
            children.insert(pid);
        }
    }
    Ok(children.into_iter().collect())
}

/// Whether a procfs read failed because its process or thread exited.
///
/// An entry that exits after `read_dir` listed it fails with `ENOENT` once
/// its directory is gone, but with `ESRCH` while the directory is still
/// open and its task is being released. Any process on the host can exit
/// mid-scan, so neither may fail the scan.
fn vanished(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

fn still_our_child(descriptor: &OwnedFd) -> io::Result<bool> {
    // SAFETY: siginfo is writable; WNOWAIT observes without reaping. An exited
    // and reaped baseline child must not exempt a later child reusing its PID.
    let mut info = unsafe { std::mem::zeroed::<libc::siginfo_t>() };
    if unsafe {
        libc::waitid(
            libc::P_PIDFD,
            descriptor.as_raw_fd().cast_unsigned(),
            &raw mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    } == -1
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ECHILD) {
            return Ok(false);
        }
        return Err(error);
    }
    Ok(true)
}

fn poll_pidfd(descriptor: &OwnedFd, timeout: Duration) {
    let Some(deadline) = Instant::now().checked_add(timeout) else {
        return;
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        let timeout_ms = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        let mut fds = [libc::pollfd {
            fd: descriptor.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        // SAFETY: `fds` is a valid pollfd array; `descriptor` stays owned.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout_ms) };
        if ready > 0 {
            return;
        }
        if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

fn owned_pidfd(pid: libc::pid_t) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open returns a new owned descriptor or -1.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if descriptor == -1 {
        return Err(io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor).map_err(io::Error::other)?;
    Ok(unsafe { OwnedFd::from_raw_fd(descriptor) })
}

fn signal_pidfd(descriptor: &OwnedFd, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: the pidfd remains live and no siginfo override is supplied.
    if unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            descriptor.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    } == -1
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum GuestEnvironment {
    Unchanged,
    WineBoot(WineMode),
    WinePreparation(WineMode),
    WineApplication(WineMode),
}

impl GuestEnvironment {
    fn apply(self, command: &mut Command) {
        let (Self::WineBoot(mode) | Self::WinePreparation(mode) | Self::WineApplication(mode)) =
            self
        else {
            return;
        };
        command.env(
            "WINEARCH",
            match mode {
                WineMode::Win32 => "win32",
                WineMode::Win64 => "win64",
            },
        );
        match self {
            Self::WineBoot(_) => {
                command
                    .env("WINEDLLOVERRIDES", "wineboot.exe=b")
                    .env_remove("WINEDLLPATH")
                    .env_remove("WINEBUILDDIR");
            }
            Self::WineApplication(_) => {
                command.env("WINEDLLOVERRIDES", crate::dxvk::NATIVE_OVERRIDES);
            }
            Self::WinePreparation(_) => {
                command.env_remove("WINEDLLOVERRIDES");
            }
            Self::Unchanged => {}
        }
    }
}

fn run_guest(
    arguments: &[OsString],
    server: &mut ManagedChild,
    environment: GuestEnvironment,
    timeout: Option<Duration>,
) -> io::Result<i32> {
    let mut command = Command::new("/usr/bin/FEX");
    command.args(arguments).process_group(0);
    configure_environment(&mut command);
    environment.apply(&mut command);
    let mut application = ManagedChild::new(command.spawn()?, true);
    let started = Instant::now();
    loop {
        // Check the server first: even a simultaneous command exit must not
        // turn an unexpectedly terminated server into successful acceptance.
        if let Some(status) = server.child.try_wait()? {
            return Err(io::Error::other(format!(
                "FEXServer exited during execution: {status}"
            )));
        }
        if let Some(status) = application.child.try_wait()? {
            return Ok(exit_code(status));
        }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Wine prefix preparation timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn configure_environment(command: &mut Command) {
    command
        .env("FEX_ROOTFS", ROOTFS)
        // FEX otherwise puts its filesystem socket below XDG_RUNTIME_DIR,
        // where the capsule grants access only to its brokered Wayland socket.
        // This absolute override is shared by the server and every client.
        // /tmp is the capsule's private tmpfs, already writable under Landlock.
        .env("FEX_SERVERSOCKETPATH", "/tmp/tilo-fex-server.sock")
        .env("FEX_INTERPRETER_INSTALLED", "0");
}

fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .unwrap_or_else(|| 128_i32.saturating_add(status.signal().unwrap_or(1)))
}

fn readiness_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    // SAFETY: pipe2 writes exactly two descriptors into the supplied array.
    if unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let [read, write] = descriptors;
    // SAFETY: both successful pipe2 results are new owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(read), OwnedFd::from_raw_fd(write)) })
}

fn wait_for_ready(pipe: &OwnedFd, server: &mut Child, timeout: Duration) -> io::Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = server.try_wait()? {
            return Err(io::Error::other(format!(
                "FEXServer exited before readiness: {status}"
            )));
        }
        if started.elapsed() >= timeout {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "FEXServer readiness timed out",
            ));
        }
        let mut event = libc::pollfd {
            fd: pipe.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the descriptor is live and event is a valid pollfd.
        let result = unsafe { libc::poll(&raw mut event, 1, 50) };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if result == 0 {
            continue;
        }
        let mut byte = 0_u8;
        // SAFETY: poll signalled a live pipe; the buffer holds one byte.
        let count = unsafe { libc::read(pipe.as_raw_fd(), (&raw mut byte).cast(), 1) };
        if count == 0 && server.try_wait()?.is_none() {
            return Ok(());
        }
        if count == -1 {
            return Err(io::Error::last_os_error());
        }
        return Err(io::Error::other("invalid FEXServer readiness response"));
    }
}

/// Ensure every failure path reaps the exact process this invocation spawned.
struct ManagedChild {
    child: Child,
    process_group: bool,
}

impl ManagedChild {
    const fn new(child: Child, process_group: bool) -> Self {
        Self {
            child,
            process_group,
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        let Ok(pid) = i32::try_from(self.child.id()) else {
            return;
        };
        // SAFETY: this child is still live and unreaped. Application groups
        // are created with process_group(0), never inherited from the caller.
        unsafe {
            libc::kill(
                if self.process_group {
                    pid.saturating_neg()
                } else {
                    pid
                },
                libc::SIGTERM,
            );
        }
        if let Ok(pidfd) = owned_pidfd(pid) {
            poll_pidfd(&pidfd, Duration::from_secs(2));
        }
        if !matches!(self.child.try_wait(), Ok(None)) {
            return;
        }
        // SAFETY: the leader remains our live unreaped child, so its process
        // group cannot have been reused for an unrelated application.
        if self.process_group {
            unsafe {
                libc::kill(pid.saturating_neg(), libc::SIGKILL);
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one isolated process owns the complete signal, adoption, cleanup, and lock-lifetime regression"
    )]
    fn exclusive_cleanup_reaps_detached_descendants_and_preserves_existing_children() {
        const CHILD: &str = "TILO_TEST_PREPARATION_REAPER";
        if std::env::var_os(CHILD).is_none() {
            // Subreaper state is process-wide. Isolate this test from the
            // parallel harness, whose unrelated children must never be adopted.
            // Stdout goes to a file, not a pipe: a descendant the child
            // preserves could otherwise hold a pipe open past the child's exit.
            let log_dir = tempfile::tempdir().unwrap();
            let log = log_dir.path().join("child.stdout");
            let status = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "translated::tests::exclusive_cleanup_reaps_detached_descendants_and_preserves_existing_children", "--nocapture"])
                .env(CHILD, "1")
                .stdout(File::create(&log).unwrap())
                .status().unwrap();
            let stdout = std::fs::read_to_string(&log).unwrap();
            assert!(status.success(), "{stdout}");
            // libtest exits 0 when `--exact` selects nothing; require the child ran.
            assert!(
                stdout.contains("test result: ok. 1 passed"),
                "the child ran no test: {stdout}"
            );
            return;
        }
        let mut disposition = unsafe { std::mem::zeroed::<libc::sigaction>() };
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &raw mut disposition) },
            0
        );
        for (handler, flags) in [(libc::SIG_IGN, 0), (libc::SIG_DFL, libc::SA_NOCLDWAIT)] {
            let mut automatic = disposition;
            automatic.sa_sigaction = handler;
            automatic.sa_flags = flags;
            assert_eq!(
                unsafe {
                    libc::sigaction(libc::SIGCHLD, &raw const automatic, std::ptr::null_mut())
                },
                0
            );
            let error = PreparationChildren::acquire()
                .err()
                .expect("auto-reaping must fail preflight");
            assert!(error.to_string().contains("waitable SIGCHLD"));
            assert_eq!(
                unsafe {
                    libc::sigaction(libc::SIGCHLD, &raw const disposition, std::ptr::null_mut())
                },
                0
            );
        }
        let temporary = tempfile::tempdir().unwrap();
        let prefix =
            advisory_lock::FileLock::try_exclusive(File::open(temporary.path()).unwrap()).unwrap();
        let competitor =
            || advisory_lock::FileLock::try_shared(File::open(temporary.path()).unwrap());
        // Killed on drop, so a failing assertion cannot leave it holding the
        // harness's stderr open after this process exits.
        let mut existing =
            ManagedChild::new(Command::new("/bin/sleep").arg("30").spawn().unwrap(), false);
        // acquire bypasses only the production single-thread check: the reexec
        // harness has an idle main thread and exactly this one test worker.
        let descendants = PreparationChildren::acquire().unwrap();
        let old_state = descendants.previous_subreaper;
        let pid_file = temporary.path().join("detached-pid");
        let script = temporary.path().join("nested.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\n/bin/sleep 30 &\nprintf '%s %s' \"$$\" \"$!\" > \"$1\"\nwait\n",
        )
        .unwrap();
        let status = Command::new("/bin/sh")
            .args([
                "-c",
                "setsid /bin/sh \"$1\" \"$2\" </dev/null >/dev/null 2>&1 &",
                "sh",
            ])
            .arg(&script)
            .arg(&pid_file)
            .status()
            .unwrap();
        assert!(status.success());
        // Starting three processes is not what this test measures; on a
        // loaded runner it takes arbitrarily long, so wait on the hang bound.
        let pids: Vec<libc::pid_t> = tilo_test_env::wait_for("the nested child to start", || {
            let contents = std::fs::read_to_string(&pid_file).ok()?;
            let values: Vec<_> = contents
                .split_whitespace()
                .map(|value| value.parse().unwrap())
                .collect();
            (values.len() == 2).then_some(values)
        });
        let pid = pids[0];
        assert_eq!(
            unsafe { libc::getsid(pid) },
            pid,
            "child must detach its session"
        );
        let detached = owned_pidfd(pid).unwrap();
        let grandchild = owned_pidfd(pids[1]).unwrap();
        assert!(still_our_child(&detached).unwrap());
        assert!(
            !still_our_child(&grandchild).unwrap(),
            "grandchild initially belongs to the detached parent"
        );
        assert!(matches!(
            competitor(),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        drop(descendants);
        assert!(
            !still_our_child(&detached).unwrap(),
            "detached writer must be reaped before unlock"
        );
        assert!(
            !still_our_child(&grandchild).unwrap(),
            "adopted grandchild must also be reaped"
        );
        let mut exited = libc::pollfd {
            fd: grandchild.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&raw mut exited, 1, 0) },
            1,
            "grandchild must have exited, not merely remain unowned"
        );
        assert!(
            existing.child.try_wait().unwrap().is_none(),
            "preexisting server must survive"
        );
        assert!(matches!(
            competitor(),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        let mut restored = -1;
        assert_eq!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &raw mut restored) },
            0
        );
        assert_eq!(restored, old_state);
        drop(existing);
        drop(prefix);
        drop(competitor().unwrap());
    }

    #[test]
    fn translated_launches_replace_inherited_overrides_and_allow_listed_modes() {
        {
            for mode in [WineMode::Win32, WineMode::Win64] {
                let mut boot = Command::new("/bin/true");
                boot.env("WINEDLLOVERRIDES", "attacker");
                GuestEnvironment::WineBoot(mode).apply(&mut boot);
                assert!(boot.get_envs().any(|(key, value)| key == "WINEDLLOVERRIDES"
                    && value == Some(std::ffi::OsStr::new("wineboot.exe=b"))));
                for key in ["WINEDLLPATH", "WINEBUILDDIR"] {
                    assert!(
                        boot.get_envs()
                            .any(|(actual, value)| actual == key && value.is_none())
                    );
                }
                let mut cleanup = Command::new("/bin/true");
                GuestEnvironment::WinePreparation(mode).apply(&mut cleanup);
                assert!(
                    cleanup
                        .get_envs()
                        .any(|(key, value)| key == "WINEDLLOVERRIDES" && value.is_none())
                );
                let mut application = Command::new("/bin/true");
                GuestEnvironment::WineApplication(mode).apply(&mut application);
                assert!(
                    application
                        .get_envs()
                        .any(|(key, value)| key == "WINEDLLOVERRIDES"
                            && value == Some(std::ffi::OsStr::new(crate::dxvk::NATIVE_OVERRIDES)))
                );
                let expected = match mode {
                    WineMode::Win32 => "win32",
                    WineMode::Win64 => "win64",
                };
                assert!(application.get_envs().any(|(key, value)| key == "WINEARCH"
                    && value == Some(std::ffi::OsStr::new(expected))));
            }
            let mut proton = Command::new("/bin/true");
            GuestEnvironment::Unchanged.apply(&mut proton);
            assert_eq!(proton.get_envs().count(), 0);
        }
        {
            let mut child = Command::new("/usr/bin/env");
            child
                .env_clear()
                .env("FEX_ROOTFS", "/attacker/rootfs")
                .env("FEX_SERVERSOCKETPATH", "/attacker/server.sock")
                .env("FEX_INTERPRETER_INSTALLED", "1")
                .env("XDG_RUNTIME_DIR", "/run/user/1000");
            configure_environment(&mut child);
            let output = child.output().unwrap();
            assert!(output.status.success());
            let environment = String::from_utf8(output.stdout).unwrap();
            assert!(
                environment
                    .lines()
                    .any(|line| line == "FEX_SERVERSOCKETPATH=/tmp/tilo-fex-server.sock")
            );
            assert!(
                environment
                    .lines()
                    .any(|line| line == format!("FEX_ROOTFS={ROOTFS}"))
            );
            assert!(
                environment
                    .lines()
                    .any(|line| line == "FEX_INTERPRETER_INSTALLED=0")
            );
            assert!(
                environment
                    .lines()
                    .any(|line| line == "XDG_RUNTIME_DIR=/run/user/1000")
            );
            assert!(!environment.contains("/attacker/"));
        }
        {
            for arguments in [
                vec![],
                vec![OsString::from("wine64")],
                vec![OsString::from("/bin/sh"), OsString::from("app.exe")],
            ] {
                assert_eq!(
                    run_translated_command(&arguments).unwrap_err().kind(),
                    io::ErrorKind::InvalidInput
                );
            }
        }
        {
            assert_eq!(exit_code(ExitStatus::from_raw(23 << 8)), 23);
            assert_eq!(
                exit_code(ExitStatus::from_raw(libc::SIGTERM)),
                128 + libc::SIGTERM
            );
        }
    }

    fn sleeping_server() -> ManagedChild {
        ManagedChild::new(
            Command::new("/bin/sleep").arg("3600").spawn().unwrap(),
            false,
        )
    }

    #[test]
    fn translated_readiness_requires_a_bounded_clean_eof_from_a_live_server() {
        {
            let (read, write) = readiness_pipe().unwrap();
            let mut server = sleeping_server();
            drop(write);
            wait_for_ready(&read, &mut server.child, tilo_test_env::hang_bound()).unwrap();
            assert!(server.child.try_wait().unwrap().is_none());
        }
        {
            let (read, _write) = readiness_pipe().unwrap();
            let mut server = sleeping_server();
            let error =
                wait_for_ready(&read, &mut server.child, Duration::from_millis(10)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        }
        {
            let (read, write) = readiness_pipe().unwrap();
            File::from(write).write_all(b"unexpected").unwrap();
            let mut server = sleeping_server();
            let error =
                wait_for_ready(&read, &mut server.child, tilo_test_env::hang_bound()).unwrap_err();
            assert!(error.to_string().contains("invalid FEXServer readiness"));
        }
        {
            let (read, write) = readiness_pipe().unwrap();
            drop(write);
            let mut server = Command::new("/bin/true").spawn().unwrap();
            server.wait().unwrap();
            let error =
                wait_for_ready(&read, &mut server, tilo_test_env::hang_bound()).unwrap_err();
            assert!(error.to_string().contains("before readiness"));
        }
    }
}
