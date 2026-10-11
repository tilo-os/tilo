//! The namespace's route out: `pasta` (from the passt project), attached to
//! it from outside.
//!
//! `pasta` runs unprivileged in the supervisor's own network namespace and
//! gives the sandbox a tap device that mirrors the host's addresses and
//! routes; every connection the sandbox makes is re-made by `pasta` from a
//! host socket. It is what rootless podman uses. Nothing is forwarded
//! inwards, and the host's loopback is not mapped in: a sandbox cannot reach
//! services that listen only on the machine itself.
//!
//! # Lifetime
//!
//! `pasta` must not outlive the sandbox, however the supervisor ends —
//! including `SIGKILL`, which is how a closed Terminal ends a served
//! session. `PR_SET_PDEATHSIG` does not survive the `SELinux` domain
//! transition into `pasta_t`, and `pasta`'s own namespace watch cannot read
//! `/proc/<pid>/ns` under that policy. So `pasta` is started by a small
//! reaper — a forked child of the supervisor that does not `exec` — which
//! blocks on a pipe only the supervisor holds open. When that pipe closes,
//! for any reason, the reaper stops `pasta` and exits.

use std::ffi::CString;
use std::io::Read;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::netns::{Netns, close_others, pipe};

/// How long `pasta` may take to configure the namespace.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// A running `pasta`, stopped when dropped (or when the supervisor dies).
#[derive(Debug)]
pub struct Pasta {
    reaper: libc::pid_t,
    release: Option<OwnedFd>,
    pid_file: PathBuf,
}

/// The `pasta` command line for `netns`, without the binary.
pub fn args(netns: &Netns, pid_file: &Path, map_host_loopback: Option<IpAddr>) -> Vec<String> {
    let pid = netns.pid();
    let mut args: Vec<String> = [
        "--config-net",
        "--quiet",
        "--foreground",
        // The namespace is held by descriptors, not a path pasta could
        // watch; the reaper stops pasta.
        "--no-netns-quit",
        // Nothing reaches into the sandbox from outside.
        "-t",
        "none",
        "-u",
        "none",
        "-T",
        "none",
        "-U",
        "none",
    ]
    .iter()
    .map(ToString::to_string)
    .collect();
    args.push("--map-host-loopback".into());
    args.push(map_host_loopback.map_or_else(|| "none".to_owned(), |a| a.to_string()));
    args.push("--pid".into());
    args.push(pid_file.display().to_string());
    args.push("--netns".into());
    args.push(format!("/proc/{pid}/ns/net"));
    args.push("--userns".into());
    args.push(format!("/proc/{pid}/ns/user"));
    args
}

/// Where `pasta` writes its pid once the namespace is configured: under
/// `$XDG_RUNTIME_DIR`, the one place `SELinux` lets it write pid files and no
/// other user can replace the pid this process later reads.
fn pid_file() -> Result<PathBuf, String> {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|d| d.is_dir())
        .ok_or("pasta needs XDG_RUNTIME_DIR for its pid file")?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!("tilo-egress-{}-{n}.pasta.pid", std::process::id())))
}

fn cstring(bytes: &[u8]) -> Result<CString, String> {
    CString::new(bytes).map_err(|_| "an argument contains a NUL byte".to_owned())
}

impl Pasta {
    /// Start `pasta` on `netns` and wait until the namespace is configured.
    ///
    /// # Errors
    /// Returns why `pasta` could not be started or did not finish setting
    /// up, with what it printed.
    pub fn start(
        binary: &Path,
        netns: &Netns,
        map_host_loopback: Option<IpAddr>,
    ) -> Result<Self, String> {
        if !binary.is_file() {
            return Err(format!(
                "{} is not installed (it is in the `passt` package)",
                binary.display()
            ));
        }
        let pid_file = pid_file()?;
        let _ = std::fs::remove_file(&pid_file);
        // Everything the children use is prepared before the fork.
        let program = cstring(binary.as_os_str().as_bytes())?;
        let pid_path = cstring(pid_file.as_os_str().as_bytes())?;
        let mut argv = vec![program.clone()];
        for a in args(netns, &pid_file, map_host_loopback) {
            argv.push(cstring(a.as_bytes())?);
        }
        let envp: Vec<CString> = std::env::vars_os()
            .filter_map(|(k, v)| {
                let mut kv = k.as_bytes().to_vec();
                kv.push(b'=');
                kv.extend_from_slice(v.as_bytes());
                CString::new(kv).ok()
            })
            .collect();
        let mut argv_ptrs: Vec<*const libc::c_char> = argv.iter().map(|a| a.as_ptr()).collect();
        argv_ptrs.push(std::ptr::null());
        let mut envp_ptrs: Vec<*const libc::c_char> = envp.iter().map(|e| e.as_ptr()).collect();
        envp_ptrs.push(std::ptr::null());

        let (release_r, release_w) = pipe().map_err(|e| e.to_string())?;
        let (err_r, err_w) = pipe().map_err(|e| e.to_string())?;
        // SAFETY: the child runs only async-signal-safe system calls on the
        // memory prepared above, and leaves through `_exit`.
        let reaper = unsafe { libc::fork() };
        if reaper < 0 {
            return Err(format!("fork: {}", std::io::Error::last_os_error()));
        }
        if reaper == 0 {
            // SAFETY: as above; this is the child.
            unsafe {
                reaper_child(
                    release_r.as_raw_fd(),
                    err_w.as_raw_fd(),
                    program.as_ptr(),
                    argv_ptrs.as_ptr(),
                    envp_ptrs.as_ptr(),
                    pid_path.as_ptr(),
                )
            }
        }
        drop(release_r);
        drop(err_w);
        let pasta = Self {
            reaper,
            release: Some(release_w),
            pid_file,
        };

        let said = Arc::new(Mutex::new(String::new()));
        let exited = Arc::new(AtomicBool::new(false));
        {
            let (said, exited) = (Arc::clone(&said), Arc::clone(&exited));
            let mut stderr = std::fs::File::from(err_r);
            // Drained for the life of pasta, so it never blocks on a full
            // pipe; kept short, for the error below. End of file means pasta
            // is gone: only it holds the write end.
            std::thread::spawn(move || {
                let mut buf = [0_u8; 1024];
                while let Ok(n) = stderr.read(&mut buf) {
                    let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                        break;
                    };
                    if let Ok(mut text) = said.lock()
                        && text.len() < 4096
                    {
                        text.push_str(&String::from_utf8_lossy(chunk));
                    }
                }
                exited.store(true, Ordering::Release);
            });
        }
        let deadline = Instant::now().checked_add(READY_TIMEOUT);
        loop {
            if std::fs::metadata(&pasta.pid_file).is_ok_and(|m| m.len() > 0) {
                return Ok(pasta);
            }
            if exited.load(Ordering::Acquire) {
                let text = said.lock().map(|t| t.trim().to_owned()).unwrap_or_default();
                return Err(format!("pasta exited before the network was up: {text}"));
            }
            if deadline.is_none_or(|d| Instant::now() > d) {
                return Err(format!(
                    "pasta did not set up the network within {}s",
                    READY_TIMEOUT.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Pasta {
    /// `pasta`'s process id, as it wrote it once the network was up.
    pub fn pid(&self) -> Option<u32> {
        std::fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    }
}

impl Drop for Pasta {
    fn drop(&mut self) {
        drop(self.release.take());
        let mut status = 0;
        // SAFETY: `reaper` is this process's child.
        unsafe { libc::waitpid(self.reaper, &raw mut status, 0) };
        let _ = std::fs::remove_file(&self.pid_file);
    }
}

/// Start `pasta`, then wait for `release` to close and stop it.
unsafe fn reaper_child(
    release: RawFd,
    err: RawFd,
    program: *const libc::c_char,
    argv: *const *const libc::c_char,
    envp: *const *const libc::c_char,
    pid_file: *const libc::c_char,
) -> ! {
    // SAFETY: raw system calls on memory prepared before the fork.
    unsafe {
        close_others([release, err]);
        let pasta = libc::fork();
        if pasta == 0 {
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            libc::dup2(null, 0);
            libc::dup2(null, 1);
            libc::dup2(err, 2);
            libc::close(release);
            if err > 2 {
                libc::close(err);
            }
            if null > 2 {
                libc::close(null);
            }
            libc::execve(program, argv, envp);
            libc::_exit(127);
        }
        libc::close(err);
        let mut byte = 0_u8;
        while libc::read(release, (&raw mut byte).cast(), 1) < 0
            && (*libc::__errno_location()) == libc::EINTR
        {}
        if pasta > 0 {
            libc::kill(pasta, libc::SIGTERM);
            let mut status = 0;
            libc::waitpid(pasta, &raw mut status, 0);
        }
        libc::unlink(pid_file);
        libc::_exit(0)
    }
}

/// `pasta` on `PATH`, else where Fedora installs it.
pub use crate::config::find_pasta as find;
