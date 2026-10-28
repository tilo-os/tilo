//! Every path the compositor's main thread asks the kernel to walk once a test
//! has said startup is over.
//!
//! On a busy machine one path lookup can wait seconds for the disk: the
//! desktop smoke once found the main loop asleep in `path_openat` for over
//! half a second under memory pressure, a frozen desktop. So after startup
//! the main loop opens, stats and lists nothing by path; that work belongs on
//! a worker, or at startup. A test proves it by arming this audit through the
//! liveness socket (`{"op":"forbid_main_thread_files"}`), driving the desktop,
//! and reading what the main thread walked (`{"op":"main_thread_files"}`).
//!
//! The audit is a seccomp filter on the main thread alone (no
//! `SECCOMP_FILTER_FLAG_TSYNC`) whose path system calls are handed, through a
//! user-notification listener, to a supervisor thread started before the
//! filter. The supervisor lets every call proceed unchanged
//! (`SECCOMP_USER_NOTIF_FLAG_CONTINUE`): it only records the main thread's,
//! with the path and the main thread's backtrace, captured while the call
//! waits for the verdict. Threads the main thread starts afterwards inherit
//! the filter; their calls are let through without a record.
//!
//! The path is read with `process_vm_readv`, and backtraces are symbolized
//! only when the record is read, on the liveness thread, so the supervisor
//! itself walks no path and never keeps the main thread waiting on the disk.
//!
//! A store asked to wait for its writer thread on the main loop (a flush:
//! the loop asleep until an fsync elsewhere finishes) is recorded the same
//! way, as the system call [`STORAGE_WAIT`] with what it waited for: no path
//! is walked, but the desktop freezes just as long.

use std::backtrace::Backtrace;
use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

/// The [`FileWalk::syscall`] of a store waiting for its writer thread on the
/// main thread.
pub const STORAGE_WAIT: &str = "storage-wait";

/// One path system call of the main thread after the audit was armed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileWalk {
    /// The system call (`openat`, `newfstatat`, …).
    pub syscall: String,
    /// The path it named, as the main thread passed it.
    pub path: String,
    /// How many times the main thread made this same call.
    pub count: u64,
    /// Where the main thread was when it first made it, innermost frame
    /// first, or why there is no backtrace.
    pub backtrace: String,
}

impl FileWalk {
    /// The code that walks the path, stable across runs and builds: the
    /// innermost frame that is not the standard library or libc, without
    /// generic arguments, closure numbers or a symbol hash. The same code is
    /// reached from several loop handlers (a paint, an app pump, a deadline),
    /// so the site names the code, and [`Self::handler`] the way in.
    #[must_use]
    pub fn site(&self) -> String {
        let frames = self.frames();
        let storage_wait = self.syscall == STORAGE_WAIT;
        frames
            .iter()
            .find(|frame| !is_runtime_frame(frame) && (!storage_wait || !is_storage_frame(frame)))
            .cloned()
            .unwrap_or_else(|| "?".into())
    }

    /// The loop handler the walk ran under: the shell frame just inside the
    /// outermost `native::run`.
    #[must_use]
    pub fn handler(&self) -> String {
        let frames = self.frames();
        frames
            .iter()
            .rposition(|frame| frame == "shell::compositor::native::run")
            .and_then(|run| {
                frames
                    .iter()
                    .take(run)
                    .rev()
                    .find(|frame| frame.starts_with("shell::") || frame.starts_with("<shell::"))
            })
            .cloned()
            .unwrap_or_else(|| "?".into())
    }

    fn frames(&self) -> Vec<String> {
        self.backtrace
            .lines()
            .filter_map(|line| {
                let (number, symbol) = line.trim_start().split_once(": ")?;
                number.parse::<u32>().ok()?;
                Some(normalize_frame(symbol.trim()))
            })
            .collect()
    }
}

/// The standard library, libc and the dynamic loader: frames that say how a
/// path was walked, never which code walked it.
fn is_runtime_frame(frame: &str) -> bool {
    const RUNTIME: &[&str] = &[
        "std::", "<std::", "core::", "<core::", "alloc::", "<alloc::", "__", "_dl_", "_nss_",
        "nss_", "_IO_", "<&", "<[",
    ];
    const LIBC: &[&str] = &[
        "syscall",
        "statx",
        "stat",
        "stat64",
        "fstatat",
        "fstatat64",
        "lstat",
        "open",
        "open64",
        "openat",
        "openat64",
        "opendir",
        "readlink",
        "readlinkat",
        "access",
        "faccessat",
        "mkdir",
        "mkdirat",
        "unlink",
        "unlinkat",
        "rename",
        "renameat",
        "renameat2",
        "fopen",
        "fopen64",
        "inotify_add_watch",
        "realpath",
        "getpwnam_r",
        "getgrnam_r",
        "getpwuid_r",
        "getgrgid_r",
        "dlopen",
    ];
    RUNTIME.iter().any(|prefix| frame.starts_with(prefix)) || LIBC.contains(&frame)
}

/// The audit and the store itself: frames that say how a storage wait was
/// reported, never which code asked for it.
fn is_storage_frame(frame: &str) -> bool {
    const STORAGE: &[&str] = &[
        "shell::compositor::liveness::",
        "durable_store::",
        "<durable_store::",
        "app_state::fs_store::",
        "<app_state::fs_store::",
    ];
    STORAGE.iter().any(|prefix| frame.starts_with(prefix))
}

/// `frame` without generic arguments, closure numbers or a symbol hash.
fn normalize_frame(frame: &str) -> String {
    let frame = frame
        .rsplit_once("::h")
        .filter(|(_, hash)| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        .map_or(frame, |(symbol, _)| symbol);
    let mut out = String::with_capacity(frame.len());
    let mut characters = frame.chars().peekable();
    // How deep inside a `::<…>` generic argument list the walk is.
    let mut generics = 0_usize;
    while let Some(character) = characters.next() {
        if generics > 0 {
            match character {
                '<' => generics = generics.saturating_add(1),
                '>' => generics = generics.saturating_sub(1),
                _ => {}
            }
            continue;
        }
        if character == ':' && out.ends_with(':') && characters.peek() == Some(&'<') {
            out.pop();
            characters.next();
            generics = 1;
            continue;
        }
        out.push(character);
    }
    let mut normalized = String::with_capacity(out.len());
    let mut rest = out.as_str();
    while let Some((before, after)) = rest.split_once("{closure#") {
        normalized.push_str(before);
        normalized.push_str("{closure}");
        rest = after.split_once('}').map_or("", |(_, tail)| tail);
    }
    normalized.push_str(rest);
    normalized
}

struct Recorded {
    syscall: &'static str,
    path: String,
    count: u64,
    backtrace: Option<String>,
}

/// Whether the audit is armed, or why it could not be.
static ARMED: OnceLock<Result<(), String>> = OnceLock::new();
static RECORD: Mutex<Vec<Recorded>> = Mutex::new(Vec::new());

/// `AUDIT_ARCH_*` of the native system call table: a call made through
/// another table is not watched by number and passes.
#[cfg(target_arch = "x86_64")]
const NATIVE_ARCH: u32 = 0xC000_003E;
#[cfg(target_arch = "aarch64")]
const NATIVE_ARCH: u32 = 0xC000_00B7;

/// The system calls that walk a path, with the index of their path argument.
fn watched() -> Vec<(libc::c_long, &'static str, usize)> {
    let calls = vec![
        (libc::SYS_openat, "openat", 1),
        (libc::SYS_openat2, "openat2", 1),
        (libc::SYS_newfstatat, "newfstatat", 1),
        (libc::SYS_statx, "statx", 1),
        (libc::SYS_faccessat, "faccessat", 1),
        (libc::SYS_faccessat2, "faccessat2", 1),
        (libc::SYS_readlinkat, "readlinkat", 1),
        (libc::SYS_mkdirat, "mkdirat", 1),
        (libc::SYS_unlinkat, "unlinkat", 1),
        (libc::SYS_renameat2, "renameat2", 1),
        (libc::SYS_inotify_add_watch, "inotify_add_watch", 1),
    ];
    #[cfg(target_arch = "x86_64")]
    let calls = calls
        .into_iter()
        .chain([
            (libc::SYS_open, "open", 0),
            (libc::SYS_creat, "creat", 0),
            (libc::SYS_stat, "stat", 0),
            (libc::SYS_lstat, "lstat", 0),
            (libc::SYS_access, "access", 0),
            (libc::SYS_readlink, "readlink", 0),
            (libc::SYS_mkdir, "mkdir", 0),
            (libc::SYS_unlink, "unlink", 0),
            (libc::SYS_rename, "rename", 0),
            (libc::SYS_renameat, "renameat", 1),
        ])
        .collect();
    calls
}

/// Arm the audit. Runs on the main thread, which it watches from then on;
/// a second call answers as the first did.
///
/// # Errors
///
/// When called off the main thread, or when the kernel refuses the filter or
/// its listener: the audit cannot observe the main thread then, and a test
/// relying on it must fail.
pub(super) fn arm() -> Result<(), String> {
    ARMED.get_or_init(install).clone()
}

/// What the main thread walked since the audit was armed, in the order first
/// seen.
///
/// # Errors
///
/// When the audit is not armed, or failed to arm.
pub(super) fn report() -> Result<Vec<FileWalk>, String> {
    match ARMED.get() {
        Some(Ok(())) => {}
        Some(Err(error)) => return Err(error.clone()),
        None => return Err("the main-thread file audit was never armed".into()),
    }
    Ok(RECORD
        .lock()
        .iter()
        .map(|walk| FileWalk {
            syscall: walk.syscall.to_owned(),
            path: walk.path.clone(),
            count: walk.count,
            backtrace: walk.backtrace.clone().unwrap_or_else(|| {
                "no backtrace: the main thread did not answer the capture signal".into()
            }),
        })
        .collect())
}

/// A store is about to wait for its writer thread on this thread (the
/// [`durable_store::waiting`] observer). Recorded as a [`STORAGE_WAIT`]
/// while the audit is armed and this is the main thread.
pub(super) fn record_storage_wait(what: &str) {
    // SAFETY: `getpid` takes no arguments and cannot fail.
    if !matches!(ARMED.get(), Some(Ok(()))) || super::gettid() != unsafe { libc::getpid() } {
        return;
    }
    let backtrace = Backtrace::force_capture();
    let mut record = RECORD.lock();
    if let Some(seen) = record
        .iter_mut()
        .find(|walk| walk.syscall == STORAGE_WAIT && walk.path == what)
    {
        seen.count = seen.count.saturating_add(1);
        return;
    }
    record.push(Recorded {
        syscall: STORAGE_WAIT,
        path: what.to_owned(),
        count: 1,
        backtrace: Some(backtrace.to_string()),
    });
}

fn install() -> Result<(), String> {
    // SAFETY: `getpid` takes no arguments and cannot fail.
    let main = unsafe { libc::getpid() };
    if super::gettid() != main {
        return Err("the main-thread file audit is armed on the main thread".into());
    }
    super::install_capture_handler();
    let calls = watched();
    // Started before the filter goes on, the supervisor is not watched.
    let (listener_tx, listener_rx) = std::sync::mpsc::channel::<libc::c_int>();
    let supervisor_calls = calls.clone();
    std::thread::Builder::new()
        .name("tilo-file-audit".into())
        .spawn(move || {
            if let Ok(listener) = listener_rx.recv() {
                supervise(listener, main, &supervisor_calls);
            }
        })
        .map_err(|error| format!("starting the file audit's supervisor: {error}"))?;
    let listener = install_filter(&calls)?;
    listener_tx
        .send(listener)
        .map_err(|_| "the file audit's supervisor stopped".to_string())?;
    Ok(())
}

/// Put the filter on this thread, and answer its notification listener.
fn install_filter(calls: &[(libc::c_long, &'static str, usize)]) -> Result<libc::c_int, String> {
    let fits = |what: &str| format!("the file audit's filter: {what} out of range");
    let statement = |code: u32, k: u32| -> Result<libc::sock_filter, String> {
        Ok(libc::sock_filter {
            code: u16::try_from(code).map_err(|_| fits("a BPF opcode"))?,
            jt: 0,
            jf: 0,
            k,
        })
    };
    let arch_offset = u32::try_from(std::mem::offset_of!(libc::seccomp_data, arch))
        .map_err(|_| fits("the arch offset"))?;
    let mut filters = vec![
        statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, arch_offset)?,
        // The native table goes on to the numbers; any other passes.
        libc::sock_filter {
            jt: 1,
            ..statement(libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K, NATIVE_ARCH)?
        },
        statement(libc::BPF_RET | libc::BPF_K, libc::SECCOMP_RET_ALLOW)?,
        statement(libc::BPF_LD | libc::BPF_W | libc::BPF_ABS, 0)?,
    ];
    for (index, (number, _, _)) in calls.iter().enumerate() {
        filters.push(libc::sock_filter {
            // On a match, skip the remaining comparisons and the allow.
            jt: u8::try_from(calls.len().saturating_sub(index)).map_err(|_| fits("a jump"))?,
            ..statement(
                libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K,
                u32::try_from(*number).map_err(|_| fits("a system call number"))?,
            )?
        });
    }
    filters.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_ALLOW,
    )?);
    filters.push(statement(
        libc::BPF_RET | libc::BPF_K,
        libc::SECCOMP_RET_USER_NOTIF,
    )?);
    let program = libc::sock_fprog {
        len: u16::try_from(filters.len()).map_err(|_| fits("the program length"))?,
        filter: filters.as_mut_ptr(),
    };
    // SAFETY: scalar prctl and seccomp calls on the current thread. The kernel
    // copies the program while `filters` is live. The filter grants nothing
    // and denies nothing: every call it catches proceeds once the supervisor
    // has seen it.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return Err(format!(
                "no_new_privs for the file audit: {}",
                std::io::Error::last_os_error()
            ));
        }
        let listener = libc::syscall(
            libc::SYS_seccomp,
            libc::SECCOMP_SET_MODE_FILTER,
            libc::SECCOMP_FILTER_FLAG_NEW_LISTENER,
            &raw const program,
        );
        libc::c_int::try_from(listener)
            .ok()
            .filter(|fd| *fd >= 0)
            .ok_or_else(|| {
                format!(
                    "installing the file audit's filter: {}",
                    std::io::Error::last_os_error()
                )
            })
    }
}

/// Answer every notification on `listener` by letting the call proceed, and
/// record the main thread's.
fn supervise(listener: libc::c_int, main: i32, calls: &[(libc::c_long, &'static str, usize)]) {
    // The call whose backtrace was just captured: the capture signal
    // interrupts it, and the kernel restarts it as a new notification.
    let mut restarted: Option<(libc::c_int, String)> = None;
    loop {
        // SAFETY: a zeroed `seccomp_notif` is the documented input of RECV.
        let mut notification: libc::seccomp_notif = unsafe { std::mem::zeroed() };
        // SAFETY: `listener` is the filter's listener; `notification` lives
        // across the call.
        let received = unsafe {
            libc::ioctl(
                listener,
                libc::SECCOMP_IOCTL_NOTIF_RECV,
                &raw mut notification,
            )
        };
        if received != 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                // A notification whose caller went away, or a signal here.
                Some(libc::ENOENT | libc::EINTR) => continue,
                _ => return,
            }
        }
        if i32::try_from(notification.pid).ok() == Some(main) {
            note_main_call(&notification, main, calls, &mut restarted);
        }
        let mut response = libc::seccomp_notif_resp {
            id: notification.id,
            val: 0,
            error: 0,
            flags: u32::try_from(libc::SECCOMP_USER_NOTIF_FLAG_CONTINUE).unwrap_or(1),
        };
        // SAFETY: `response` names the notification just received. A call
        // interrupted meanwhile answers ENOENT, which is fine: it restarts.
        unsafe {
            libc::ioctl(listener, libc::SECCOMP_IOCTL_NOTIF_SEND, &raw mut response);
        }
    }
}

fn note_main_call(
    notification: &libc::seccomp_notif,
    main: i32,
    calls: &[(libc::c_long, &'static str, usize)],
    restarted: &mut Option<(libc::c_int, String)>,
) {
    let nr = notification.data.nr;
    let Some(&(_, name, argument)) = calls
        .iter()
        .find(|(number, _, _)| libc::c_long::from(nr) == *number)
    else {
        return;
    };
    let pointer = notification.data.args.get(argument).copied().unwrap_or(0);
    let path = read_path(main, pointer);
    // `statx` and `newfstatat` of a descriptor (`AT_EMPTY_PATH`) walk nothing.
    if path.is_empty() {
        return;
    }
    if restarted
        .take()
        .is_some_and(|(seen, seen_path)| seen == nr && seen_path == path)
    {
        return;
    }
    let first = {
        let mut record = RECORD.lock();
        if let Some(walk) = record
            .iter_mut()
            .find(|walk| walk.syscall == name && walk.path == path)
        {
            walk.count = walk.count.saturating_add(1);
            false
        } else {
            record.push(Recorded {
                syscall: name,
                path: path.clone(),
                count: 1,
                backtrace: None,
            });
            true
        }
    };
    if !first {
        return;
    }
    let backtrace = match super::capture_held_thread(main) {
        Ok(frames) => {
            *restarted = Some((nr, path.clone()));
            frames
        }
        Err(why) => why,
    };
    if let Some(walk) = RECORD
        .lock()
        .iter_mut()
        .find(|walk| walk.syscall == name && walk.path == path)
    {
        walk.backtrace = Some(backtrace);
    }
}

/// The NUL-terminated path at `pointer` in this process, read without
/// trusting the pointer: a page at a time, so a string ending just before an
/// unmapped page still reads.
fn read_path(pid: i32, pointer: u64) -> String {
    const PAGE: u64 = 4096;
    const PATH_MAX: usize = 4096;
    let mut bytes = Vec::with_capacity(256);
    let mut at = pointer;
    while bytes.len() < PATH_MAX && at != 0 {
        let chunk = usize::try_from(PAGE.saturating_sub(at % PAGE)).unwrap_or(1);
        let mut buffer = vec![0_u8; chunk];
        let local = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: chunk,
        };
        let remote = libc::iovec {
            iov_base: at as *mut libc::c_void,
            iov_len: chunk,
        };
        // SAFETY: reads `chunk` bytes of this process's memory into
        // `buffer`, which is that long; the kernel checks the remote range.
        let read =
            unsafe { libc::process_vm_readv(pid, &raw const local, 1, &raw const remote, 1, 0) };
        let Ok(read) = usize::try_from(read) else {
            break;
        };
        buffer.truncate(read);
        if let Some(end) = buffer.iter().position(|byte| *byte == 0) {
            bytes.extend(buffer.iter().take(end));
            return String::from_utf8_lossy(&bytes).into_owned();
        }
        bytes.extend_from_slice(&buffer);
        if read < chunk {
            break;
        }
        at = at.saturating_add(u64::try_from(read).unwrap_or(PAGE));
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(backtrace: &str) -> FileWalk {
        FileWalk {
            syscall: "statx".into(),
            path: "/x".into(),
            count: 1,
            backtrace: backtrace.into(),
        }
    }

    #[test]
    fn a_site_names_the_code_and_the_handler_it_runs_under() {
        let walk = walk(
            "   3: statx\n   4: std::sys::fs::unix::try_statx\n   5: std::fs::metadata::<&std::path::Path>\n\
             \x20  6: <tilo_identity_host::registry::DirStamp>::of\n\
             \x20            at ./crates/runtime/tilo-identity-host/src/registry.rs:234:20\n\
             \x20  7: <shell::shell::network::ShellNetwork>::kick_periodic_sync\n\
             \x20  8: <shell::shell::network::ShellNetwork>::frame_tick\n\
             \x20  9: shell::compositor::native::run\n  10: shell::linux_main::run\n  11: main",
        );
        assert_eq!(walk.site(), "<tilo_identity_host::registry::DirStamp>::of");
        assert_eq!(
            walk.handler(),
            "<shell::shell::network::ShellNetwork>::frame_tick"
        );
    }

    #[test]
    fn a_site_drops_generics_closure_numbers_and_hashes() {
        assert_eq!(
            normalize_frame(
                "settings_store::watch::watch_settings_at::<<shell::X as y::Z>::f::{closure#0}>::{closure#3}::h0123456789abcdef"
            ),
            "settings_store::watch::watch_settings_at::{closure}"
        );
    }
}
