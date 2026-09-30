//! Spawning the Tilo terminal app to run a command.
//!
//! "Enter distrobox" (and the container Terminal tab's "Attach shell") opens
//! the standard Tilo `terminal` app, which spawns the program named by the
//! `SHELL` environment variable in a PTY (see `apps/terminal`,
//! `TerminalBackend::new`). The terminal treats `SHELL` as a bare program
//! path with no arguments, so to run `distrobox enter <box>` we:
//!
//!   1. write a tiny launcher script that `exec`s the target command and
//!      then drops to an interactive shell (so the window stays usable after
//!      the command finishes), and
//!   2. spawn the `terminal` binary with `SHELL` pointed at that launcher.
//!
//! The argv/command assembly is split into pure helpers
//! ([`distrobox_enter_command`], [`terminal_binary`], [`build_launcher`]) so
//! the wiring is unit-testable headlessly.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The shell command run *inside* the terminal to enter a distrobox. Kept
/// pure for testing: it execs `distrobox enter <box>` and, when that returns,
/// falls back to an interactive login shell so the window remains useful.
pub fn distrobox_enter_command(box_name: &str) -> String {
    // Single-quote the box name to survive odd characters in the launcher.
    let quoted = shell_single_quote(box_name);
    format!("distrobox enter {quoted}; exec \"${{SHELL_FALLBACK:-/bin/bash}}\"")
}

/// The command run inside the terminal to attach an interactive shell to a
/// running container (the detail Terminal tab). Pure for testing.
pub fn exec_attach_command(container_id: &str, shell: &str) -> String {
    let id = shell_single_quote(container_id);
    let sh = shell_single_quote(shell);
    format!("podman exec -it {id} {sh}; exec \"${{SHELL_FALLBACK:-/bin/bash}}\"")
}

/// Single-quote a value for safe interpolation into a `/bin/sh -c` string.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Resolve the `terminal` binary: prefer the sibling binary in the same
/// directory as the current executable (the fleet `target/debug/terminal`),
/// then fall back to `terminal` on `PATH`.
pub fn terminal_binary() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let sibling = dir.join("terminal");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("terminal")
}

/// Write a launcher script that runs `command` then drops to a shell, and
/// return its path. The script is created executable. `dir` is the directory
/// to write into (a per-user runtime dir at call sites).
///
/// # Errors
///
/// Returns `Err` when `dir` can't be created, the script can't be written,
/// or (on Unix) its executable bit can't be set.
pub fn build_launcher(dir: &Path, stem: &str, command: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("tilo-term-{stem}.sh"));
    // `SHELL_FALLBACK` lets the launched command's `exec "$SHELL_FALLBACK"`
    // resolve to the user's real shell even though we overrode `SHELL` to
    // point at this script. Default to bash for the interactive fallback.
    let script =
        format!("#!/bin/sh\nexport SHELL_FALLBACK=\"${{SHELL_FALLBACK:-/bin/bash}}\"\n{command}\n");
    std::fs::write(&path, script)?;
    bounded_command::make_executable(&path)?;
    Ok(path)
}

/// Per-user runtime dir for launcher scripts: `$XDG_RUNTIME_DIR/tilo-containers`
/// or a temp fallback.
pub fn runtime_dir() -> PathBuf {
    xdg_paths::xdg::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-containers")
}

/// Build (but do not spawn) the `Command` that launches the Tilo terminal
/// running `inner_command`. Exposed for testing the wiring without spawning.
///
/// # Errors
///
/// Returns `Err` when the launcher script can't be written (see
/// [`build_launcher`]).
pub fn terminal_command(inner_command: &str, stem: &str) -> std::io::Result<Command> {
    let launcher = build_launcher(&runtime_dir(), stem, inner_command)?;
    let mut cmd = Command::new(terminal_binary());
    // The terminal reads SHELL as the program to spawn in its PTY. Capture the
    // user's real shell first so the launcher's fallback still works.
    let real_shell = std::env::var_os("SHELL").unwrap_or_else(|| OsString::from("/bin/bash"));
    cmd.env("SHELL_FALLBACK", real_shell);
    cmd.env("SHELL", launcher.as_os_str());
    Ok(cmd)
}

/// Spawn a Tilo terminal entering the named distrobox. Returns an error if the
/// launcher can't be written or the terminal can't be spawned.
///
/// # Errors
///
/// Returns `Err` when the launcher script can't be written (see
/// [`build_launcher`]) or the `terminal` binary can't be spawned.
pub fn spawn_distrobox_enter(box_name: &str) -> std::io::Result<()> {
    let inner = distrobox_enter_command(box_name);
    let stem = sanitize_stem(box_name);
    let mut cmd = terminal_command(&inner, &format!("enter-{stem}"))?;
    cmd.spawn().map(|_| ())
}

/// Spawn a Tilo terminal attaching an interactive shell to a container.
///
/// # Errors
///
/// Returns `Err` when the launcher script can't be written (see
/// [`build_launcher`]) or the `terminal` binary can't be spawned.
pub fn spawn_exec_attach(container_id: &str, shell: &str) -> std::io::Result<()> {
    let inner = exec_attach_command(container_id, shell);
    let stem = sanitize_stem(container_id);
    let mut cmd = terminal_command(&inner, &format!("exec-{stem}"))?;
    cmd.spawn().map(|_| ())
}

/// Reduce an identifier to a filename-safe stem.
fn sanitize_stem(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = s.trim_matches('-');
    let n = trimmed.len().min(24);
    // Every char here was normalised to ASCII above, so byte offset `n`
    // always lands on a char boundary; `.get()` is just the panic-safe form.
    trimmed.get(..n).unwrap_or(trimmed).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Distrobox enter and exec attach commands are built and quoted safely, and the launcher is executable, sets the shell and has a safe stem.
    #[test]
    fn terminal_commands_quote_and_launch_safely() {
        // --- phase: enter_command_runs_distrobox_enter_then_shell ---
        {
            let cmd = distrobox_enter_command("my-distrobox");
            assert!(cmd.contains("distrobox enter 'my-distrobox'"), "{cmd}");
            // Falls back to an interactive shell so the window stays usable.
            assert!(cmd.contains("exec"), "{cmd}");
        }

        // --- phase: enter_command_quotes_hostile_box_name ---
        {
            let cmd = distrobox_enter_command("a'; rm -rf ~ #");
            // The injected quote is escaped, not left to break out of the string.
            assert!(
                cmd.contains(r"'\''"),
                "hostile name must be single-quote-escaped: {cmd}"
            );
            assert!(
                !cmd.contains("rm -rf ~ #;"),
                "no unescaped command break: {cmd}"
            );
        }

        // --- phase: exec_attach_targets_the_container_and_shell ---
        {
            let cmd = exec_attach_command("9f3c1a7be4d2", "/bin/sh");
            assert!(
                cmd.contains("podman exec -it '9f3c1a7be4d2' '/bin/sh'"),
                "{cmd}"
            );
        }

        // --- phase: launcher_is_written_executable_and_runs_command ---
        {
            let dir = std::env::temp_dir().join("tilo-containers-test-launcher");
            let _ = std::fs::remove_dir_all(&dir);
            let path =
                build_launcher(&dir, "unit", "echo hi; exec \"$SHELL_FALLBACK\"").expect("write");
            assert!(path.exists());
            let body = std::fs::read_to_string(&path).unwrap();
            assert!(body.starts_with("#!/bin/sh"), "launcher is a sh script");
            assert!(body.contains("echo hi"), "launcher embeds the command");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "launcher is executable");
            }
            let _ = std::fs::remove_dir_all(&dir);
        }

        // --- phase: terminal_command_sets_shell_to_launcher ---
        {
            let cmd = terminal_command("echo test", "unit-tc").expect("build");
            let envs: Vec<_> = cmd.get_envs().collect();
            let shell = envs
                .iter()
                .find(|(k, _)| *k == std::ffi::OsStr::new("SHELL"))
                .and_then(|(_, v)| *v)
                .expect("SHELL env set");
            let shell = shell.to_string_lossy();
            assert!(
                shell.contains("tilo-term-unit-tc.sh"),
                "SHELL points at the launcher: {shell}"
            );
            // The real shell is preserved as the fallback.
            assert!(
                envs.iter()
                    .any(|(k, _)| *k == std::ffi::OsStr::new("SHELL_FALLBACK")),
                "SHELL_FALLBACK preserved"
            );
        }

        // --- phase: sanitize_stem_is_filename_safe ---
        {
            assert_eq!(sanitize_stem("my/box:1.0"), "my-box-1-0");
            assert_eq!(sanitize_stem("---a---"), "a");
        }
    }
}
