//! The SDK for Tilo terminal apps: stdio, workspace paths, and the same
//! portal and workspace services a GUI app reaches, from a process that owns
//! its terminal and may be piped.
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
//! It exposes no grant API: the launcher's sandbox and `collab-types::caps`
//! decide what a CLI app reaches, as for a GUI app.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
pub use runtime_portal::NotAvailable;
pub use runtime_portal::is_tilo_native;

pub mod output;
pub mod workspace;

pub use output::{OutputBuilder, output};
pub use workspace::{
    WorkspaceHandle, act, is_workspace_available, resolve_workspace_path, workspace_root,
};

/// The CLI command name the shim invoked us as; `None` outside the launcher.
pub fn invoked_command() -> Option<String> {
    std::env::var("TILO_CLI_COMMAND").ok()
}

/// Run a fallible closure as `main`: ignores `SIGPIPE` so `notes | head` does
/// not panic on a closed pipe, prints the anyhow chain to stderr, and exits
/// `1` on error, `0` on `Ok`.
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

/// The process's package id from `TILO_PACKAGE_ID`, falling back to a dev default.
pub fn package_id() -> String {
    std::env::var("TILO_PACKAGE_ID").unwrap_or_else(|_| "tilo-standalone".to_string())
}

/// Like `fs::create_dir_all`, but the error names the offending path.
pub fn ensure_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("creating directory {}: {e}", dir.display()))
}

/// The directory each package's state directory is a child of, which lets a
/// surface enumerate everything stored.
pub fn data_home() -> PathBuf {
    // An empty or relative XDG_DATA_HOME is invalid by the spec, not a path
    // to resolve against the working directory.
    if let Some(xdg) = tilo_paths::env_dir("XDG_DATA_HOME") {
        return xdg;
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local);
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return Path::new(&profile).join("AppData/Local");
        }
    }
    if let Some(home) = tilo_paths::home() {
        return home.join(".local/share");
    }
    // Temporary storage is not persistent and is used when the platform's
    // home/data environment is unavailable. The page has neither: a bare
    // relative path fails every file operation there instead of panicking.
    tilo_paths::temp_dir().unwrap_or_default()
}

/// Where a package outside Tilo keeps state: an app may point its own
/// storage here too.
pub fn standalone_state_dir(pkg_id: &str) -> PathBuf {
    standalone_state_dir_in(&data_home(), pkg_id)
}

/// [`standalone_state_dir`] below an explicit data home instead of the one
/// the environment names, for callers (and tests) that own their root.
pub fn standalone_state_dir_in(home: &Path, pkg_id: &str) -> PathBuf {
    home.join(pkg_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_environment_names_the_command_package_and_state_dir() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_CLI_COMMAND", "notes");
        assert_eq!(invoked_command().as_deref(), Some("notes"));
        env.remove("TILO_CLI_COMMAND");
        assert!(invoked_command().is_none());
        env.remove("TILO_PACKAGE_ID");
        assert_eq!(package_id(), "tilo-standalone");
        env.set("TILO_PACKAGE_ID", "dev.example.notes");
        assert_eq!(package_id(), "dev.example.notes");
        env.set("XDG_DATA_HOME", "/tmp/xdg");
        assert_eq!(
            standalone_state_dir("notes.tilo.sh"),
            PathBuf::from("/tmp/xdg/notes.tilo.sh")
        );
    }

    #[test]
    fn run_main_maps_ok_to_zero_and_err_to_one() {
        let cases: [(&str, anyhow::Result<()>, u8); 2] =
            [("ok", Ok(()), 0), ("err", Err(anyhow::anyhow!("boom")), 1)];
        for (name, result, want) in cases {
            let code = run_main(move || result);
            assert_eq!(
                format!("{code:?}"),
                format!("{:?}", ExitCode::from(want)),
                "case {name}"
            );
        }
    }
}
