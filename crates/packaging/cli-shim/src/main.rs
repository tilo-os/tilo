//! qos-cli-shim — the PATH-facing wrapper installed once per CLI
//! command a qOS package publishes.
//!
//! Reads `argv[0]` (bare command or full path), locates its own
//! executable path via [`std::env::current_exe`], reads the sidecar
//! `<exe-path>.qos-shim` (TOML: `package_id`, `command`, optional
//! `launcher`), and `execv`s the launcher:
//! ```text
//! qos-launcher run-installed --package <pkg-id> --command <cmd> -- <argv[1..]>
//! ```
//! The sidecar lets one shim binary serve every installed command
//! without recompiling per install; its one open/read/close is
//! dwarfed by the launcher's bwrap fork anyway.
//!
//! ### Security posture
//!
//! - The sidecar sits in the user's own data dir and is only writable
//!   by `install` — write access there already means owning the account.
//! - `LD_PRELOAD`/`LD_LIBRARY_PATH` are NOT inherited specially; the
//!   launcher runs unsandboxed and reconstructs a clean env for the
//!   sandboxed entrypoint.
//! - The sidecar's `launcher` override exists only for out-of-tree
//!   testing; production installs leave it unset.

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

use std::env;
use std::ffi::CString;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The suffix appended to the shim executable's path to find its
/// sidecar; a `const` so `install` and the shim agree.
pub const SIDECAR_SUFFIX: &str = ".qos-shim";

/// System install path for the launcher; overridable by the sidecar's
/// `launcher = ...` field so tests can point at a target/debug/qos-launcher without root-installing.
const DEFAULT_LAUNCHER_PATH: &str = "/usr/libexec/qos-launcher";

/// Wire form of the sidecar TOML.
#[derive(Debug, Deserialize)]
struct Sidecar {
    /// Reverse-DNS package id (`dev.qos.notes-cli`).
    package_id: String,
    /// Command name as declared in the manifest's [cli].commands.
    /// Duplicated here (argv[0] also carries it) so renaming the shim
    /// doesn't silently rebind to a different command.
    command: String,
    /// Optional override for the launcher binary path; falls back to [`DEFAULT_LAUNCHER_PATH`] when unset.
    #[serde(default)]
    launcher: Option<PathBuf>,
}

fn main() -> ! {
    // Diverges — either execvs the launcher (never returns) or exits via the error path with a status the caller can inspect.
    let exe = env::current_exe().unwrap_or_else(|e| {
        die(&format!("qos-cli-shim: cannot resolve current exe: {e}"));
    });
    let sidecar_path = sidecar_path_for(&exe);
    let sidecar = read_sidecar(&sidecar_path).unwrap_or_else(|e| {
        die(&format!(
            "qos-cli-shim: sidecar {} unreadable: {e}",
            sidecar_path.display()
        ));
    });

    let launcher = sidecar
        .launcher
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LAUNCHER_PATH));
    let mut passthrough: Vec<String> = env::args().skip(1).collect();

    // Build the argv the launcher expects. The `--` separator matters:
    // everything after it is opaque user args forwarded verbatim —
    // without it clap would try to interpret --help etc. as launcher flags.
    let mut argv: Vec<String> = Vec::with_capacity(6 + passthrough.len());
    argv.push(launcher.file_name().map_or_else(
        || "qos-launcher".to_string(),
        |s| s.to_string_lossy().into_owned(),
    ));
    argv.push("run-installed".to_string());
    argv.push("--package".to_string());
    argv.push(sidecar.package_id);
    argv.push("--command".to_string());
    argv.push(sidecar.command);
    argv.push("--".to_string());
    argv.append(&mut passthrough);

    execv_launcher(&launcher, &argv);
}

/// Resolve the sidecar path from the shim's own executable path; public so `install` and tests agree on the layout.
pub fn sidecar_path_for(exe: &Path) -> PathBuf {
    let mut p = exe.to_path_buf();
    let name = p.file_name().map_or_else(
        || "qos-cli-shim".to_string(),
        |s| s.to_string_lossy().into_owned(),
    );
    p.set_file_name(format!("{name}{SIDECAR_SUFFIX}"));
    p
}

fn read_sidecar(path: &Path) -> Result<Sidecar, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    toml::from_str::<Sidecar>(text).map_err(|e| e.to_string())
}

fn execv_launcher(launcher: &Path, argv: &[String]) -> ! {
    // Build the raw C argv + terminating NULL.
    let path_c = CString::new(launcher.as_os_str().as_encoded_bytes())
        .unwrap_or_else(|_| die("qos-cli-shim: launcher path contains a NUL byte"));
    let owned: Vec<CString> = argv
        .iter()
        .map(|s| {
            CString::new(s.as_bytes())
                .unwrap_or_else(|_| die("qos-cli-shim: argv contains a NUL byte"))
        })
        .collect();
    let mut c_argv: Vec<*const libc::c_char> = owned.iter().map(|s| s.as_ptr()).collect();
    c_argv.push(std::ptr::null());
    // SAFETY: path_c is a nul-terminated string; c_argv is a
    // null-terminated array of nul-terminated strings; both live at
    // least until the syscall returns (which it only does on error).
    unsafe {
        libc::execv(path_c.as_ptr(), c_argv.as_ptr());
    }
    // execv only returns on failure.
    let errno = std::io::Error::last_os_error();
    die(&format!(
        "qos-cli-shim: execv {} failed: {errno}",
        launcher.display()
    ));
}

fn die(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(127);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn sidecar_path_suffixes_the_exe_name() {
        let p = sidecar_path_for(Path::new("/home/x/.local/share/qos-apps/bin/notes"));
        assert_eq!(
            p,
            PathBuf::from("/home/x/.local/share/qos-apps/bin/notes.qos-shim")
        );
    }

    #[test]
    fn sidecar_deserialises() {
        let text = r#"
package_id = "dev.qos.notes-cli"
command    = "notes"
"#;
        let s: Sidecar = toml::from_str(text).expect("decode");
        assert_eq!(s.package_id, "dev.qos.notes-cli");
        assert_eq!(s.command, "notes");
        assert!(s.launcher.is_none());
    }

    #[test]
    fn sidecar_launcher_override_deserialises() {
        let text = r#"
package_id = "dev.qos.notes-cli"
command    = "notes"
launcher   = "/tmp/target/debug/qos-launcher"
"#;
        let s: Sidecar = toml::from_str(text).expect("decode");
        assert_eq!(
            s.launcher.as_deref(),
            Some(Path::new("/tmp/target/debug/qos-launcher"))
        );
    }
}
