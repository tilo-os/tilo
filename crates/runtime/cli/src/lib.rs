//! `cli` — SDK for qOS CLI apps.
//!
//! qOS apps come in two flavours: GUI (talks Wayland, uses the
//! `ui` toolkit) and CLI (owns stdio, prints text, may be piped).
//! Both flavours run inside the same launcher-managed sandbox,
//! and both talk to the same underlying services (workspace fs,
//! collab broker, portal). This crate bundles the CLI-side
//! ergonomics into a single dependency so a new terminal app is
//! a few dozen lines.
//!
//! ## Entry point
//!
//! ```ignore
//! use cli::{run_main, resolve_workspace_path};
//!
//! fn main() -> std::process::ExitCode {
//!     run_main(|| {
//!         let path = resolve_workspace_path("notes/inbox.md")?;
//!         std::fs::write(path, "hello\n")?;
//!         Ok(())
//!     })
//! }
//! ```
//!
//! [`run_main`] wraps the closure with:
//!
//! - `anyhow::Error` → stderr + non-zero exit code.
//! - Silent handling of `SIGPIPE` (e.g. `| head`).
//! - A `QOS_CLI_COMMAND` env var lookup, exposed via
//!   [`invoked_command`].
//!
//! ## Workspace paths & graceful degradation
//!
//! [`resolve_workspace_path`] returns a filesystem path a CLI
//! can `open()` today. Two cases:
//!
//! 1. Inside qOS ([`is_qos_native`]): under `/var/workspace/`.
//! 2. Standalone (stock Linux, Flatpak, etc.): under
//!    `~/.local/share/notes.qos.dev/` (or whatever the current
//!    process's package id is; falls back to
//!    `~/.local/share/qos-standalone/`). Deliberately unrelated
//!    to `runtime-fs` so the CLI keeps working when the FUSE
//!    driver is offline.
//!
//! ## Portal grants
//!
//! This SDK exposes **no** grant API. Access control for a CLI
//! app is the launcher's declarative sandbox (`sandbox.fs.read` /
//! `sandbox.fs.write` / `sandbox.net`) plus the capability model
//! in `collab-types::caps`, both enforced outside the app. A
//! `request_grant` helper that returned `Ok(())` without asking
//! anyone used to live here; a permission call that always
//! succeeds is worse than no permission call, so it is gone. When
//! the compositor grows a text/dbus portal, add the real request
//! here — one that can actually say no.
//!
//! ## Output helpers
//!
//! [`output`] builds a terminal-aware writer that emits ANSI OSC
//! 8 hyperlinks when the destination is an interactive terminal
//! (matches how gnome-terminal / iTerm2 / kitty render clickable
//! paths), degrading to plain text on pipes.

use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
pub use runtime::NotAvailable;
pub use runtime::is_qos_native;

pub mod output;
pub mod workspace;

pub use output::{OutputBuilder, output};
pub use workspace::{
    WorkspaceHandle, act, is_workspace_available, resolve_workspace_path, workspace_root,
};

/// The name of the CLI command the shim invoked us as. `None`
/// when we're running outside the launcher (or the launcher is
/// older than the `run-installed` subcommand).
pub fn invoked_command() -> Option<String> {
    std::env::var("QOS_CLI_COMMAND").ok()
}

/// Run a fallible closure as `main` — mirrors the ergonomics of
/// `main() -> ExitCode` while allowing `?` on any `anyhow::Error`.
///
/// Behavioural details:
/// - Ignores `SIGPIPE` on Unix so `notes | head` doesn't panic
///   the CLI when the reader closes the pipe.
/// - Prints the anyhow chain to stderr via `{:#}` for a
///   compact one-line message including all causes.
/// - Exits with `1` on error, `0` on `Ok`.
pub fn run_main<F: FnOnce() -> Result<()>>(f: F) -> ExitCode {
    #[cfg(unix)]
    // SAFETY: signal handler install is one-shot per process;
    // no other thread is touching signal(3) at startup.
    unsafe {
        libc_signal_ignore_sigpipe();
    }
    match f() {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: {e:#}");
            ExitCode::from(1)
        }
    }
}

#[cfg(unix)]
unsafe fn libc_signal_ignore_sigpipe() {
    // Avoid pulling in a full libc dependency here — the SDK is
    // deliberately minimal — via a raw sigaction syscall through
    // libc's C ABI. If the SIG_IGN install fails we silently
    // continue; the CLI just returns a panic-y error on broken
    // pipes, which is what the pre-SDK behaviour was.
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_IGN: usize = 1;
    let _ = unsafe { signal(SIGPIPE, SIG_IGN) };
}

/// Convenience wrapper: pick the process's package id from
/// `QOS_PACKAGE_ID` (set by the launcher). Falls back to a
/// well-known default for local dev / testing.
pub fn package_id() -> String {
    std::env::var("QOS_PACKAGE_ID").unwrap_or_else(|_| "qos-standalone".to_string())
}

/// Print a value to stdout if it's a terminal, else write it
/// unstyled. Small enough to skip the `output` module for
/// simple hello-world code paths.
pub fn println_terminal_aware(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let _ = out.is_terminal(); // ensure the trait is imported at call sites
    writeln!(out, "{text}")
}

/// Ensure the given directory exists — mirrors `fs::create_dir_all`
/// but returns an `anyhow::Error` with the offending path in the
/// error chain, which makes CLI errors much friendlier.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("creating directory {}: {e}", dir.display()))
}

/// The XDG data dir helper the SDK uses to place fallback state
/// when running outside qOS. Public so an app can point its
/// own storage at the same spot.
pub fn standalone_state_dir(pkg_id: &str) -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg).join(pkg_id);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join(".local/share").join(pkg_id);
    }
    // Last-ditch: /tmp. Not persistent, but keeps the CLI alive
    // rather than crashing on `HOME unset`.
    Path::new("/tmp").join(pkg_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[serial_test::serial]
    fn invoked_command_reads_env() {
        unsafe { std::env::set_var("QOS_CLI_COMMAND", "notes") };
        assert_eq!(invoked_command().as_deref(), Some("notes"));
        unsafe { std::env::remove_var("QOS_CLI_COMMAND") };
        assert!(invoked_command().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn package_id_falls_back_to_standalone() {
        unsafe { std::env::remove_var("QOS_PACKAGE_ID") };
        assert_eq!(package_id(), "qos-standalone");
        unsafe { std::env::set_var("QOS_PACKAGE_ID", "dev.example.notes") };
        assert_eq!(package_id(), "dev.example.notes");
        unsafe { std::env::remove_var("QOS_PACKAGE_ID") };
    }

    #[test]
    #[serial_test::serial]
    fn standalone_state_dir_honours_xdg() {
        unsafe { std::env::set_var("XDG_DATA_HOME", "/tmp/xdg") };
        assert_eq!(
            standalone_state_dir("notes.qos.dev"),
            PathBuf::from("/tmp/xdg/notes.qos.dev")
        );
        unsafe { std::env::remove_var("XDG_DATA_HOME") };
    }

    #[test]
    fn run_main_returns_zero_on_ok() {
        let code = run_main(|| Ok(()));
        // ExitCode isn't PartialEq, but we can format it.
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(0)));
    }

    #[test]
    fn run_main_returns_one_on_err() {
        let code = run_main(|| Err(anyhow::anyhow!("boom")));
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::from(1)));
    }
}
