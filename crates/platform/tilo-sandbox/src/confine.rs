//! What a helper process does at start-up, before it reads untrusted input.
//!
//! 1. [`detach`]: close every inherited descriptor except stdio, and arrange
//!    to die with the parent.
//! 2. Load whatever code and data it needs (a parser library).
//! 3. [`limit_resources`]: no core dumps, a small descriptor budget.
//! 4. [`confine`]: read-only access to [`Policy::read_only`] and nothing
//!    else — no writes, no network, no `exec`. "No network" means no
//!    socket can be bound or connected: Linux refuses the `socket` call
//!    itself, macOS refuses `bind` and `connect`.
//!
//! A confinement the platform cannot fully enforce is an error
//! ([`ConfineError::NotEnforced`]); callers decide whether to refuse to run
//! or to parse in process, and never run a half-confined helper as if it
//! were confined.

use std::path::PathBuf;

/// What a confined helper may still do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Policy {
    /// Files and directories (recursively) the helper may read.
    pub read_only: Vec<PathBuf>,
}

/// Why a start-up step failed.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfineError {
    /// A process set-up step failed (descriptors, parent watch, limits).
    #[error("{0}")]
    Setup(String),
    /// The confinement could not be installed, or only partially.
    #[error("{0}")]
    NotEnforced(String),
    /// This target has no helper processes to confine.
    #[error("process sandboxing is not available on this platform")]
    Unsupported,
}

/// Close inherited descriptors above stderr and die with the parent.
///
/// # Errors
///
/// [`ConfineError::Setup`] when either cannot be arranged, including when
/// the parent has already exited.
pub fn detach() -> Result<(), ConfineError> {
    sys::close_inherited_descriptors().map_err(ConfineError::Setup)?;
    sys::die_with_parent().map_err(ConfineError::Setup)
}

/// Disable core dumps and cap open descriptors at `max_open_files`.
///
/// # Errors
///
/// [`ConfineError::Setup`] when a limit cannot be set.
pub fn limit_resources(max_open_files: u64) -> Result<(), ConfineError> {
    sys::limit_resources(max_open_files).map_err(ConfineError::Setup)
}

/// Confine the calling process to `policy`, irreversibly, for it and every
/// descendant. Never call this from a process that must keep running
/// unconfined (a test harness).
///
/// # Errors
///
/// [`ConfineError::NotEnforced`] when the platform refuses the policy or can
/// enforce only part of it; [`ConfineError::Unsupported`] on the web.
pub fn confine(policy: &Policy) -> Result<(), ConfineError> {
    sys::confine(policy)
}

#[cfg(unix)]
fn set_limits(max_open_files: u64) -> Result<(), String> {
    for (resource, value) in [
        (libc::RLIMIT_CORE, 0),
        (libc::RLIMIT_NOFILE, max_open_files as libc::rlim_t),
    ] {
        let limit = libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        };
        // SAFETY: setrlimit with a valid struct.
        if unsafe { libc::setrlimit(resource, &raw const limit) } != 0 {
            return Err(format!("setrlimit: {}", std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
mod sys {
    use std::io;

    use super::{ConfineError, Policy};

    pub fn die_with_parent() -> Result<(), String> {
        // SAFETY: prctl with integer arguments.
        let rc = unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) };
        if rc != 0 {
            return Err(format!("PR_SET_PDEATHSIG: {}", io::Error::last_os_error()));
        }
        // The parent may have exited before the call took effect.
        // SAFETY: getppid has no failure mode.
        if unsafe { libc::getppid() } == 1 {
            return Err("the parent exited".to_string());
        }
        Ok(())
    }

    pub fn close_inherited_descriptors() -> Result<(), String> {
        // SAFETY: close_range on descriptors this process does not use.
        let rc = unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) };
        if rc != 0 {
            return Err(format!("close_range: {}", io::Error::last_os_error()));
        }
        Ok(())
    }

    pub fn limit_resources(max_open_files: u64) -> Result<(), String> {
        super::set_limits(max_open_files)
    }

    pub fn confine(policy: &Policy) -> Result<(), ConfineError> {
        confine_filesystem(policy).map_err(ConfineError::NotEnforced)?;
        confine_syscalls().map_err(ConfineError::NotEnforced)
    }

    fn confine_filesystem(policy: &Policy) -> Result<(), String> {
        use sandbox_core::landlock::{
            FS_ABI, LandlockOutcome, NetRestriction, RulesetSpec, access_plan, outcome_from_status,
            restrict_self,
        };
        let spec = RulesetSpec::application(
            access_plan(&policy.read_only, &[], FS_ABI),
            NetRestriction::DenyAll,
        );
        let status = restrict_self(&spec, |_, _| {}).map_err(|e| format!("landlock: {e}"))?;
        match outcome_from_status(&status.ruleset) {
            LandlockOutcome::FullyEnforced => Ok(()),
            other => Err(other.describe().to_string()),
        }
    }

    fn confine_syscalls() -> Result<(), String> {
        let program = sandbox_core::seccomp::parser_program().map_err(|e| e.to_string())?;
        sandbox_core::seccomp::install(&program).map_err(|e| e.to_string())
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::{CStr, CString, c_char};
    use std::io;
    use std::path::Path;

    use super::{ConfineError, Policy};

    unsafe extern "C" {
        /// `<sandbox.h>`: with flags 0, `profile` is Seatbelt profile source.
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> i32;
        fn sandbox_free_error(errorbuf: *mut c_char);
    }

    /// macOS has no parent-death signal: a thread waits on a kqueue for the
    /// parent's exit and ends the process.
    pub fn die_with_parent() -> Result<(), String> {
        // SAFETY: getppid has no failure mode.
        let parent = unsafe { libc::getppid() };
        if parent == 1 {
            return Err("the parent exited".to_string());
        }
        // SAFETY: kqueue has no arguments; the descriptor is owned below.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(format!("kqueue: {}", io::Error::last_os_error()));
        }
        let ident =
            usize::try_from(parent).map_err(|_| "the parent pid is negative".to_string())?;
        let watch = libc::kevent {
            ident,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: registers one event from a valid struct; no events out.
        let rc = unsafe {
            libc::kevent(
                queue,
                &raw const watch,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: closing the descriptor created above.
            unsafe { libc::close(queue) };
            // ESRCH: the parent is already gone.
            return Err(format!("watching the parent: {err}"));
        }
        std::thread::Builder::new()
            .name("parent-watch".into())
            .spawn(move || {
                // SAFETY: an all-zero kevent is a valid output buffer.
                let mut fired: libc::kevent = unsafe { std::mem::zeroed() };
                loop {
                    // SAFETY: waits for one event into `fired`.
                    let n = unsafe {
                        libc::kevent(
                            queue,
                            std::ptr::null(),
                            0,
                            &raw mut fired,
                            1,
                            std::ptr::null(),
                        )
                    };
                    if n > 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                        // SAFETY: _exit is async-signal-safe and takes no pointers.
                        unsafe { libc::_exit(1) };
                    }
                }
            })
            .map_err(|e| format!("parent watch thread: {e}"))?;
        Ok(())
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the Linux implementation, where close_range can fail"
    )]
    pub fn close_inherited_descriptors() -> Result<(), String> {
        // SAFETY: getdtablesize has no failure mode.
        let table = unsafe { libc::getdtablesize() }.clamp(3, 1 << 16);
        for fd in 3..table {
            // SAFETY: closing descriptors this process does not use; EBADF
            // for the unused slots is expected.
            unsafe { libc::close(fd) };
        }
        Ok(())
    }

    pub fn limit_resources(max_open_files: u64) -> Result<(), String> {
        super::set_limits(max_open_files)
    }

    pub fn confine(policy: &Policy) -> Result<(), ConfineError> {
        let profile = CString::new(profile(policy))
            .map_err(|_| ConfineError::NotEnforced("a path contains NUL".into()))?;
        let mut error: *mut c_char = std::ptr::null_mut();
        // SAFETY: `profile` is NUL-terminated; `error` receives a string
        // the library allocates, freed below.
        let rc = unsafe { sandbox_init(profile.as_ptr(), 0, &raw mut error) };
        if rc == 0 {
            return Ok(());
        }
        let message = if error.is_null() {
            "sandbox_init failed".to_string()
        } else {
            // SAFETY: a non-null error is a NUL-terminated string.
            let text = unsafe { CStr::from_ptr(error) }
                .to_string_lossy()
                .into_owned();
            // SAFETY: freeing the buffer sandbox_init allocated.
            unsafe { sandbox_free_error(error) };
            text
        };
        Err(ConfineError::NotEnforced(format!("seatbelt: {message}")))
    }

    /// The Seatbelt profile for `policy`: deny by default; read system code
    /// (already-loaded libraries may map more of it lazily), the policy's
    /// paths, and metadata; create and use only this crate's shared memory
    /// objects. No writes, sockets, `exec`, `fork` or Mach services.
    pub fn profile(policy: &Policy) -> String {
        let mut profile = String::from(
            "(version 1)\n\
             (deny default)\n\
             (allow signal (target self))\n\
             (allow sysctl-read)\n\
             (allow file-read-metadata)\n\
             (allow file-read* (literal \"/dev/null\") (literal \"/dev/urandom\")\n\
               (subpath \"/usr/lib\") (subpath \"/System/Library/Frameworks\")\n\
               (subpath \"/System/Library/PrivateFrameworks\"))\n\
             (allow file-write-data (literal \"/dev/null\"))\n\
             (allow ipc-posix-shm-read-data ipc-posix-shm-write-create\n\
               ipc-posix-shm-write-data ipc-posix-shm-write-unlink\n\
               (ipc-posix-name-regex #\"^/tilo-\"))\n",
        );
        let paths: Vec<String> = policy
            .read_only
            .iter()
            .filter_map(|path| {
                let resolved = path.canonicalize().ok()?;
                let quoted = quote(&resolved)?;
                Some(if resolved.is_dir() {
                    format!("(subpath {quoted})")
                } else {
                    format!("(literal {quoted})")
                })
            })
            .collect();
        if !paths.is_empty() {
            profile.push_str("(allow file-read* ");
            profile.push_str(&paths.join(" "));
            profile.push_str(")\n");
        }
        profile
    }

    fn quote(path: &Path) -> Option<String> {
        let text = path.to_str()?;
        let mut out = String::with_capacity(text.len().saturating_add(2));
        out.push('"');
        for ch in text.chars() {
            if matches!(ch, '"' | '\\') {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push('"');
        Some(out)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod sys {
    use super::{ConfineError, Policy};

    pub fn die_with_parent() -> Result<(), String> {
        Err(ConfineError::Unsupported.to_string())
    }

    pub fn close_inherited_descriptors() -> Result<(), String> {
        Err(ConfineError::Unsupported.to_string())
    }

    pub fn limit_resources(_max_open_files: u64) -> Result<(), String> {
        Err(ConfineError::Unsupported.to_string())
    }

    pub fn confine(_policy: &Policy) -> Result<(), ConfineError> {
        Err(ConfineError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Set in the re-executed test binary: the probe below confines itself
    /// and reports what it could still do through its exit status.
    const PROBE_ENV: &str = "TILO_SANDBOX_PROBE_DIR";

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn confined_child_reads_only_its_paths() {
        if let Some(dir) = std::env::var_os(PROBE_ENV) {
            #[allow(
                clippy::exit,
                reason = "the re-executed probe reports through its exit status"
            )]
            std::process::exit(probe(&PathBuf::from(dir)));
        }
        let root = std::env::temp_dir().join(format!("tilo-sandbox-probe-{}", std::process::id()));
        let allowed = root.join("allowed");
        let denied = root.join("denied");
        std::fs::create_dir_all(&allowed).unwrap();
        std::fs::create_dir_all(&denied).unwrap();
        std::fs::write(allowed.join("a"), b"a").unwrap();
        std::fs::write(denied.join("d"), b"d").unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "confine::tests::confined_child_reads_only_its_paths",
            ])
            .env(PROBE_ENV, &root)
            .status()
            .unwrap();
        let _ = std::fs::remove_dir_all(&root);
        if cfg!(target_os = "linux") && status.code() == Some(PROBE_UNENFORCEABLE) {
            eprintln!("skipped: this kernel cannot enforce the policy");
            return;
        }
        assert_eq!(
            status.code(),
            Some(0),
            "probe failures (bit mask): {status}"
        );
    }

    const PROBE_UNENFORCEABLE: i32 = 100;

    fn probe(root: &std::path::Path) -> i32 {
        use std::os::unix::net::UnixDatagram;
        let allowed = root.join("allowed");
        let policy = Policy {
            read_only: vec![allowed.clone()],
        };
        if detach().is_err() {
            return 200;
        }
        match confine(&policy) {
            Ok(()) => {}
            Err(ConfineError::NotEnforced(message)) => {
                eprintln!("{message}");
                return PROBE_UNENFORCEABLE;
            }
            Err(_) => return 201,
        }
        let mut failures = 0;
        if std::fs::read(allowed.join("a")).is_err() {
            failures |= 1;
        }
        if std::fs::read(root.join("denied/d")).is_ok() {
            failures |= 2;
        }
        if std::fs::write(allowed.join("new"), b"x").is_ok() {
            failures |= 4;
        }
        if std::net::TcpStream::connect("127.0.0.1:9").is_ok()
            || std::net::UdpSocket::bind("127.0.0.1:0").is_ok()
        {
            failures |= 8;
        }
        // Seatbelt lets a process create sockets but not bind or connect
        // them; seccomp refuses the socket itself. Either way, no endpoint.
        if UnixDatagram::bind(allowed.join("sock")).is_ok() {
            failures |= 16;
        }
        if std::process::Command::new("/bin/echo").status().is_ok() {
            failures |= 32;
        }
        if crate::shm::from_bytes("probe", b"ok").is_err() {
            failures |= 1 << 6;
        }
        failures
    }
}
