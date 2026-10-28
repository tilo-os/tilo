//! An application must survive losing the reader of its own stderr.
//!
//! Applications are launched with stderr on a pipe the compositor reads. When
//! that reading end goes away the next `eprintln!` returns `EPIPE`, the
//! standard library turns that into a panic, and the release profile's
//! `panic = "abort"` turns the panic into `SIGABRT` — leaving windows on
//! screen whose processes no longer exist.
//!
//! The reproduction runs in a child process: the libtest harness installs an
//! output capture that `eprintln!` checks *before* it reaches file descriptor
//! 2, so an in-process test cannot make it fail, and the thing asserted (the
//! process dies) only a process can show.

#![allow(
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;

use tilo_lock::MutexRecover as _;
use tilo_ui::diag;

/// Set on the child to select which write it should attempt.
const ROLE: &str = "TILO_BROKEN_PIPE_ROLE";

/// The name libtest filters on: the module path minus the crate root that
/// `module_path!()` includes. These files are modules of one `it` binary, so
/// the bare function name matches nothing.
fn test_path(name: &str) -> String {
    match module_path!().split_once("::") {
        Some((_, module)) => format!("{module}::{name}"),
        None => name.to_string(),
    }
}

/// Re-run this test binary as a child whose stderr is a pipe with no reader,
/// and report how it ended.
fn app_whose_log_reader_left(role: &str, test_name: &str) -> ExitStatus {
    let exe = std::env::current_exe().expect("path to this test binary");
    let (reader, writer) = std::io::pipe().expect("a pipe for the child's stderr");
    // The compositor's relay stops reading. This is the moment that turns
    // every later diagnostic in the child into a failed write; it happens
    // before the child starts, so every write it makes meets it.
    drop(reader);
    let mut child = Command::new(exe)
        .args([
            &test_path(test_name),
            "--exact",
            "--nocapture",
            "--test-threads",
            "1",
        ])
        .env(ROLE, role)
        .stdout(Stdio::null())
        .stderr(writer)
        .spawn()
        .expect("spawn the child");
    child.wait().expect("wait for the child")
}

/// Returns `false` in the parent, so the test body goes on to spawn a
/// child. It never returns in the child — the write either kills it or
/// `exit(0)` does — the caller's `return` on `true` is only a guard
/// against that changing.
#[allow(
    clippy::exit,
    reason = "the child process communicates whether the write killed it purely via exit code; \
              this is the reproduction's whole point"
)]
fn played_child_role(role: &str, write: impl FnOnce()) -> bool {
    if std::env::var(ROLE).as_deref() != Ok(role) {
        return false;
    }
    // A test binary must be built with unwinding, so the release profile's
    // `panic = "abort"` is reproduced with a hook that aborts — without it
    // the child would exit 101 and the signal would go unasserted.
    std::panic::set_hook(Box::new(|_| std::process::abort()));
    write();
    // Reached only if the write did not kill us.
    std::process::exit(0);
}

/// The reproduction. Without it a passing `diag!` case could just as well be a
/// pipe that was never broken.
#[test]
fn eprintln_aborts_the_app_when_the_reader_of_stderr_is_gone() {
    if played_child_role("eprintln", || {
        eprintln!("[app] broadcast doc delta {}B", 128);
    }) {
        return;
    }

    let status = app_whose_log_reader_left(
        "eprintln",
        "eprintln_aborts_the_app_when_the_reader_of_stderr_is_gone",
    );
    assert_eq!(
        status.signal(),
        Some(libc::SIGABRT),
        "eprintln! on a pipe with no reader must abort the process \
         (this is the crash being fixed); got {status:?}"
    );
}

/// The fix: the same write, on the same broken pipe, through the path
/// applications actually use, and the process lives.
#[test]
fn diag_lets_the_app_live_when_the_reader_of_stderr_is_gone() {
    if played_child_role("diag", || diag!("[app] broadcast doc delta {}B", 128)) {
        return;
    }

    let status = app_whose_log_reader_left(
        "diag",
        "diag_lets_the_app_live_when_the_reader_of_stderr_is_gone",
    );
    assert_eq!(
        status.signal(),
        None,
        "a diagnostic line must never be able to kill the application; \
         it was killed by a signal instead: {status:?}"
    );
    assert!(
        status.success(),
        "the application should have run to completion, got {status:?}"
    );
}

// The fix must still deliver the line when there is someone to read it.
// These run in-process: `diag!` writes to `io::stderr()` directly, so unlike
// `eprintln!` it is not intercepted by the harness's capture.

/// Serialises the tests below: file descriptor 2 is process-wide.
static STDERR_SLOT: Mutex<()> = Mutex::new(());

fn with_stderr_pointed_at<R>(writer_fd: i32, body: impl FnOnce() -> R) -> R {
    // SAFETY: raw fd juggling on a descriptor this process owns, serialised by
    // `STDERR_SLOT` so no other test observes fd 2 mid-swap.
    let real_stderr = unsafe { libc::dup(2) };
    assert!(real_stderr >= 0, "could not save the real stderr");
    assert!(
        unsafe { libc::dup2(writer_fd, 2) } >= 0,
        "could not install the stand-in stderr"
    );
    let outcome = body();
    assert!(
        unsafe { libc::dup2(real_stderr, 2) } >= 0,
        "could not restore the real stderr"
    );
    unsafe { libc::close(real_stderr) };
    outcome
}

#[test]
fn diag_delivers_whole_lines_when_stderr_works() {
    // Case `diag_writes_the_line_when_stderr_works`.
    {
        use std::io::Read;

        let _guard = STDERR_SLOT.lock_recover();
        let (mut reader, writer) = std::io::pipe().expect("pipe for the stand-in stderr");

        with_stderr_pointed_at(writer.as_raw_fd(), || {
            diag!("[app] recv document {}B", 64);
        });
        drop(writer);

        let mut buf = [0u8; 256];
        let n = reader.read(&mut buf).expect("read the diagnostic back");
        assert_eq!(&buf[..n], b"[app] recv document 64B\n");
    }

    // Case `a_diagnostic_line_reaches_the_pipe_whole`.
    // Not tidiness: the compositor's relay decodes this stderr as UTF-8, and a
    // line torn between two halves of a multi-byte character fails that decode
    // — a relay that stops on a decode error closes the reading end, which is
    // the other way an application loses the reader of its stderr.
    {
        use std::io::Read;

        let _guard = STDERR_SLOT.lock_recover();
        let (mut reader, writer) = std::io::pipe().expect("pipe for the stand-in stderr");
        let writer_fd = writer.into_raw_fd();

        with_stderr_pointed_at(writer_fd, || {
            // Multi-byte, so a torn write shows up as invalid UTF-8 rather than
            // merely as a short line.
            diag!("{}", "→→→→→→→→");
        });
        unsafe { libc::close(writer_fd) };

        let mut buf = [0u8; 256];
        let n = reader.read(&mut buf).expect("read the diagnostic back");
        let text = std::str::from_utf8(&buf[..n]).expect("the line is valid UTF-8");
        assert_eq!(text, "→→→→→→→→\n");
    }
}
