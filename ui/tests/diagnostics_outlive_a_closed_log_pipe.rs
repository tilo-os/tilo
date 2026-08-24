//! An application must survive losing the reader of its own stderr.
//!
//! Every qOS application is launched by the compositor with its stderr on a
//! pipe the compositor reads. When that reading end goes away while the
//! application is still running, the next `eprintln!` returns `EPIPE`, the
//! standard library turns that into a panic, and the release/demo profile's
//! `panic = "abort"` turns the panic into `SIGABRT`. Applications died
//! exactly this way during a live session — `coredumpctl` recorded
//!
//! ```text
//! thread 'main' panicked at library/std/src/io/stdio.rs:1166:9:
//! failed printing to stderr: Broken pipe (os error 32)
//! ```
//!
//! for the file explorer, the word processor and chat, and the person was
//! left looking at windows whose processes no longer existed.
//!
//! The reproduction runs in a child process, for two reasons. The libtest
//! harness installs an output capture that `println!` / `eprintln!` check
//! *before* they ever reach file descriptor 2, so an in-process test cannot
//! make them fail. And the thing being asserted is that the process dies,
//! which only a process can demonstrate.

use std::os::fd::{AsRawFd, IntoRawFd};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;

use ui::diag;

/// Set on the child to select which write it should attempt.
const ROLE: &str = "QOS_BROKEN_PIPE_ROLE";

/// Re-run this test binary as a child whose stderr is a pipe with no
/// reader, and report how it ended.
///
/// The child runs exactly one test — the caller's own — with the harness's
/// capture turned off, so the write under test reaches file descriptor 2
/// the way it does in a real application.
fn app_whose_log_reader_left(role: &str, test_name: &str) -> ExitStatus {
    let exe = std::env::current_exe().expect("path to this test binary");
    let mut child = Command::new(exe)
        .args([test_name, "--exact", "--nocapture", "--test-threads", "1"])
        .env(ROLE, role)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the child");
    // The compositor's relay stops reading. This is the moment that turns
    // every later diagnostic in the child into a failed write.
    drop(child.stderr.take());
    child.wait().expect("wait for the child")
}

/// The child half: wait until the parent has certainly closed the reading
/// end, then make the write under test.
///
/// Returns `true` when this process is the child, so the test body knows to
/// stop rather than go on to spawn one of its own.
fn played_child_role(role: &str, write: impl FnOnce()) -> bool {
    if std::env::var(ROLE).as_deref() != Ok(role) {
        return false;
    }
    // `panic = "abort"` is what the demo and release profiles carry, and it
    // is what turned these panics into SIGABRT. A test binary must be built
    // with unwinding for the harness to work at all, so the profile is
    // reproduced here instead: a hook that aborts, exactly as the abort
    // panic runtime would. Without it this child would exit 101 and the
    // signal the coredumps actually recorded would go unasserted.
    std::panic::set_hook(Box::new(|_| std::process::abort()));
    std::thread::sleep(std::time::Duration::from_millis(150));
    write();
    // Reached only if the write did not kill us.
    std::process::exit(0);
}

/// The reproduction: `eprintln!` kills an application whose log reader has
/// gone, with the signal the coredumps recorded.
///
/// This is what makes the next test mean anything. Without it, a passing
/// `diag!` case could just as well be a pipe that was never broken.
#[test]
fn eprintln_aborts_the_app_when_the_reader_of_stderr_is_gone() {
    // The shape of `ui`'s collaboration diagnostics, which is what the
    // aborting applications were writing at the time.
    if played_child_role("eprintln", || eprintln!("[app] broadcast doc delta {}B", 128)) {
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

// ---------------------------------------------------------------------
// The fix must still deliver the line when there is someone to read it.
// These run in-process: `diag!` writes to `io::stderr()` directly, so
// unlike `eprintln!` it is not intercepted by the harness's capture and
// really does reach the descriptor under test.
// ---------------------------------------------------------------------

/// Serialises the tests below: file descriptor 2 is process-wide.
static STDERR_SLOT: Mutex<()> = Mutex::new(());

/// Point the process's stderr at `writer`, run `body`, then restore it.
fn with_stderr_pointed_at<R>(writer_fd: i32, body: impl FnOnce() -> R) -> R {
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
fn diag_writes_the_line_when_stderr_works() {
    let _guard = STDERR_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    let (mut reader, writer) = std::io::pipe().expect("pipe for the stand-in stderr");

    with_stderr_pointed_at(writer.as_raw_fd(), || {
        diag!("[app] recv CRDTUpdate {}B", 64);
    });
    drop(writer);

    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = reader.read(&mut buf).expect("read the diagnostic back");
    assert_eq!(&buf[..n], b"[app] recv CRDTUpdate 64B\n");
}

/// A diagnostic line reaches the pipe as one write.
///
/// This is not tidiness. The compositor's relay decodes an application's
/// stderr as UTF-8; a line torn between two halves of a multi-byte
/// character makes that decode fail, and a relay that stops on a decode
/// error closes the reading end — which is the other way an application
/// loses the reader of its stderr.
#[test]
fn a_diagnostic_line_reaches_the_pipe_whole() {
    let _guard = STDERR_SLOT.lock().unwrap_or_else(|e| e.into_inner());
    let (mut reader, writer) = std::io::pipe().expect("pipe for the stand-in stderr");
    let writer_fd = writer.into_raw_fd();

    with_stderr_pointed_at(writer_fd, || {
        // Characters that are multi-byte, so a torn write shows up as
        // invalid UTF-8 rather than merely as a short line.
        diag!("{}", "→→→→→→→→");
    });
    unsafe { libc::close(writer_fd) };

    use std::io::Read;
    let mut buf = [0u8; 256];
    let n = reader.read(&mut buf).expect("read the diagnostic back");
    let text = std::str::from_utf8(&buf[..n]).expect("the line is valid UTF-8");
    assert_eq!(text, "→→→→→→→→\n");
}
