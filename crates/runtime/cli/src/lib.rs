//! `cli` — SDK for qOS CLI apps.
//!
//! qOS apps come in two flavours: GUI (talks Wayland, uses the `ui`
//! toolkit) and CLI (owns stdio, prints text, may be piped). Both run
//! inside the same launcher-managed sandbox and talk to the same
//! underlying services (workspace fs, collab broker, portal); this crate
//! bundles the CLI-side ergonomics so a new terminal app is a few dozen lines.
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
//! [`run_main`] wraps the closure with `anyhow::Error` → stderr + non-zero
//! exit, silent `SIGPIPE` handling (e.g. `| head`), and a `QOS_CLI_COMMAND`
//! lookup exposed via [`invoked_command`].
//!
//! [`resolve_workspace_path`] returns a path a CLI can `open()` today: under
//! `/var/workspace/` inside qOS ([`is_qos_native`]), or under
//! `~/.local/share/<package id>/` standalone (stock Linux, Flatpak) —
//! deliberately unrelated to `runtime-fs` so the CLI keeps working when the
//! FUSE driver is offline.
//!
//! This SDK exposes **no** grant API. Access control for a CLI app is the
//! launcher's sandbox (one writable directory, a fixed system read profile,
//! `sandbox.net`) plus the capability model in `collab-types::caps`, all
//! enforced outside the app — a `request_grant` helper that always returned
//! `Ok(())` used to live here, and a permission call that always succeeds is
//! worse than none, so it's gone. Add the real thing when the compositor
//! grows a text/dbus portal.
//!
//! [`output`] builds a terminal-aware writer that emits ANSI OSC 8
//! hyperlinks on an interactive terminal, degrading to plain text on pipes.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

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

/// The CLI command name the shim invoked us as; `None` outside the launcher.
pub fn invoked_command() -> Option<String> {
    std::env::var("QOS_CLI_COMMAND").ok()
}

/// Run a fallible closure as `main`, mirroring `main() -> ExitCode` while
/// allowing `?` on any `anyhow::Error`: ignores `SIGPIPE` so `notes | head`
/// doesn't panic on a closed pipe, prints the anyhow chain to stderr via
/// `{:#}`, and exits `1` on error, `0` on `Ok`.
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
    // A raw sigaction syscall through libc's C ABI, to avoid a full libc dep.
    // A failed SIG_IGN install is silently ignored — broken pipes just panic.
    unsafe extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }
    const SIGPIPE: i32 = 13;
    const SIG_IGN: usize = 1;
    let _ = unsafe { signal(SIGPIPE, SIG_IGN) };
}

/// The process's package id from `QOS_PACKAGE_ID`, falling back to a dev default.
pub fn package_id() -> String {
    std::env::var("QOS_PACKAGE_ID").unwrap_or_else(|_| "qos-standalone".to_string())
}

/// Print to stdout if it's a terminal, else write unstyled — small enough
/// to skip the `output` module for simple hello-world code paths.
pub fn println_terminal_aware(text: &str) -> io::Result<()> {
    let mut out = io::stdout().lock();
    let _ = out.is_terminal(); // ensure the trait is imported at call sites
    writeln!(out, "{text}")
}

/// Like `fs::create_dir_all`, but the error names the offending path.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("creating directory {}: {e}", dir.display()))
}

/// The directory every package's state directory is a child of, split out
/// of [`standalone_state_dir`] because "where does this package keep its
/// state" and "which packages have any" are different questions — the
/// second is what lets a surface enumerate everything stored.
pub fn data_home() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(xdg);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return Path::new(&home).join(".local/share");
    }
    // Last-ditch /tmp: not persistent, but keeps the CLI alive over HOME unset.
    PathBuf::from("/tmp")
}

/// The XDG data dir fallback for state outside qOS; public so an app can
/// point its own storage at the same spot.
pub fn standalone_state_dir(pkg_id: &str) -> PathBuf {
    data_home().join(pkg_id)
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
