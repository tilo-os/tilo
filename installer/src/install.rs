//! Backend invocation for the privileged install + power steps.
//!
//! ## Install contract (the backend ships separately, in `os/`)
//!
//! ```text
//! /usr/libexec/qos-install \
//!     --disk <dev> --user <name> \
//!     --filesystem <btrfs|ext4|xfs> \
//!     --hostname <name> [--timezone <tz>] \
//!     --keymap <xkb-layout> [--keyvariant <xkb-variant>] --yes
//! ```
//!
//! `--yes` is required because the backend has no tty here; the installer's
//! Review screen is the user's "are you sure" gate. The account password is
//! supplied on **stdin** as a single line. The backend runs the privileged
//! `bootc install to-disk` + account/hostname/timezone seeding, streams
//! human-readable progress to **stdout**, and exits `0` on success /
//! non-zero on failure. We do not implement it here — we only spawn it,
//! feed the password, and relay its output.
//!
//! `--filesystem`, `--hostname`, `--timezone`, `--keymap` and
//! `--keyvariant` are newer flags; the backend defaults them when absent,
//! so an older UI/backend pair stays compatible.
//!
//! ## Power contract
//!
//! The Finish screen's Reboot / Power-off buttons shell out to:
//!
//! ```text
//! /usr/libexec/qos-power [--reboot|--poweroff]
//! ```
//!
//! a tiny self-escalating script allowed by the same live-medium sudoers
//! drop-in. On success it never returns (the machine reboots / powers off);
//! a non-zero exit surfaces back as an error line.
//!
//! ## Threading
//!
//! [`run_install`] is synchronous and streams via the `on_line` callback.
//! The UI drives it from a **worker thread** (see
//! [`crate::installer_app`]), pumping each line back through the winit
//! `EventLoopProxy` as a `ui::UserEvent::External`, so the event loop stays
//! responsive and the status log animates live. [`stream_command`] is the
//! reusable seam both the real backend and test stubs run through.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Path to the privileged install backend installed on the live medium.
pub const INSTALL_BIN: &str = "/usr/libexec/qos-install";
/// Path to the privileged power backend (reboot / poweroff).
pub const POWER_BIN: &str = "/usr/libexec/qos-power";

/// User-selected install options threaded through to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOptions {
    /// Root filesystem: `btrfs` (default), `ext4`, or `xfs`.
    pub filesystem: String,
    /// Hostname seeded into the installed system.
    pub hostname: String,
    /// IANA time zone (e.g. `Europe/Amsterdam`). Empty ⇒ leave the
    /// backend default (UTC); the flag is then omitted.
    pub timezone: String,
    /// xkb keyboard layout (e.g. `us`, `de`). Empty ⇒ backend default
    /// (`us`); the flag is then omitted.
    pub keymap: String,
    /// Optional xkb variant (e.g. `dvorak`, `colemak`). Empty ⇒ no
    /// variant; the flag is then omitted.
    pub keyvariant: String,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
        }
    }
}

/// A terminal power action requested from the Finish screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Reboot,
    PowerOff,
}

impl PowerAction {
    fn flag(self) -> &'static str {
        match self {
            PowerAction::Reboot => "--reboot",
            PowerAction::PowerOff => "--poweroff",
        }
    }
}

/// Build the backend [`Command`] for `disk` + `user` + `opts`. stdin /
/// stdout / stderr are wired up by [`stream_command`].
pub fn install_command(disk: &str, user: &str, opts: &InstallOptions) -> Command {
    let mut cmd = Command::new(INSTALL_BIN);
    cmd.arg("--disk")
        .arg(disk)
        .arg("--user")
        .arg(user)
        .arg("--filesystem")
        .arg(&opts.filesystem)
        .arg("--hostname")
        .arg(&opts.hostname);
    if !opts.timezone.trim().is_empty() {
        cmd.arg("--timezone").arg(opts.timezone.trim());
    }
    if !opts.keymap.trim().is_empty() {
        cmd.arg("--keymap").arg(opts.keymap.trim());
        // A variant is only meaningful alongside a layout.
        if !opts.keyvariant.trim().is_empty() {
            cmd.arg("--keyvariant").arg(opts.keyvariant.trim());
        }
    }
    cmd.arg("--yes");
    cmd
}

/// Build the [`Command`] for a power action.
pub fn power_command(action: PowerAction) -> Command {
    let mut cmd = Command::new(POWER_BIN);
    cmd.arg(action.flag());
    cmd
}

/// Spawn `cmd`, write `password` (with a trailing newline) to its stdin,
/// then stream stdout line by line into `on_line`. On a non-zero exit any
/// captured stderr is relayed too (prefixed `error:`). Returns `Ok(true)`
/// iff the child exited `0`.
///
/// Separated from [`run_install`] so tests can drive it with a stub
/// command (e.g. `sh -c`) without the real backend present.
///
/// NOTE: stderr is drained only after stdout reaches EOF. A backend that
/// emits more than a pipe buffer (~64 KiB) of stderr *before* finishing
/// stdout could deadlock; the real `qos-install` writes progress to
/// stdout and only short errors to stderr, so this is safe in practice.
pub fn stream_command<F: FnMut(String)>(
    mut cmd: Command,
    password: &str,
    mut on_line: F,
) -> std::io::Result<bool> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    // Feed the password, then drop the handle so the child sees EOF.
    {
        if let Some(mut stdin) = child.stdin.take() {
            writeln!(stdin, "{password}")?;
        }
    }

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            on_line(line?);
        }
    }

    let status = child.wait()?;

    if !status.success() {
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            for line in reader.lines().map_while(Result::ok) {
                if !line.trim().is_empty() {
                    on_line(format!("error: {line}"));
                }
            }
        }
    }

    Ok(status.success())
}

/// Run the real `qos-install` backend for `disk` + `user` + `opts`, feeding
/// `password` on stdin and relaying every output line to `on_line`.
pub fn run_install<F: FnMut(String)>(
    disk: &str,
    user: &str,
    password: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    stream_command(install_command(disk, user, opts), password, on_line)
}

/// Run the power backend. Returns `Ok(true)` on a clean spawn+exit — though
/// on success the machine is already on its way down, so the UI typically
/// never observes the return.
pub fn run_power(action: PowerAction) -> std::io::Result<bool> {
    let status = power_command(action).status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_command_builds_expected_argv() {
        let opts = InstallOptions {
            filesystem: "ext4".to_string(),
            hostname: "lab-1".to_string(),
            timezone: "Europe/Amsterdam".to_string(),
            keymap: "de".to_string(),
            keyvariant: String::new(),
        };
        let cmd = install_command("/dev/sda", "quinten", &opts);
        assert_eq!(cmd.get_program(), INSTALL_BIN);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk",
                "/dev/sda",
                "--user",
                "quinten",
                "--filesystem",
                "ext4",
                "--hostname",
                "lab-1",
                "--timezone",
                "Europe/Amsterdam",
                "--keymap",
                "de",
                "--yes",
            ]
        );
    }

    #[test]
    fn install_command_passes_keyvariant_with_keymap() {
        // Dvorak = xkb layout "us" + variant "dvorak".
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: "dvorak".to_string(),
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk", "/dev/vda", "--user", "qq", "--filesystem", "btrfs", "--hostname",
                "qos", "--keymap", "us", "--keyvariant", "dvorak", "--yes",
            ]
        );
    }

    #[test]
    fn install_command_omits_empty_timezone_and_keymap() {
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: String::new(),
            keyvariant: String::new(),
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk", "/dev/vda", "--user", "qq", "--filesystem", "btrfs", "--hostname",
                "qos", "--yes",
            ]
        );
        assert!(!args.iter().any(|a| *a == "--timezone"));
        assert!(!args.iter().any(|a| *a == "--keymap"));
        assert!(!args.iter().any(|a| *a == "--keyvariant"));
    }

    #[test]
    fn install_command_omits_keyvariant_when_no_keymap() {
        // A stray variant without a layout must not be emitted alone.
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: String::new(),
            keyvariant: "dvorak".to_string(),
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(!args.iter().any(|a| *a == "--keyvariant"));
        assert!(!args.iter().any(|a| *a == "--keymap"));
    }

    #[test]
    fn default_options_are_btrfs_qos_us() {
        let o = InstallOptions::default();
        assert_eq!(o.filesystem, "btrfs");
        assert_eq!(o.hostname, "qos");
        assert!(o.timezone.is_empty());
        assert_eq!(o.keymap, "us");
        assert!(o.keyvariant.is_empty());
    }

    #[test]
    fn power_command_builds_expected_argv() {
        let reboot = power_command(PowerAction::Reboot);
        assert_eq!(reboot.get_program(), POWER_BIN);
        let args: Vec<&str> = reboot.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["--reboot"]);

        let off = power_command(PowerAction::PowerOff);
        let args: Vec<&str> = off.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(args, vec!["--poweroff"]);
    }

    #[test]
    fn stream_command_reads_password_and_collects_stdout() {
        // Stub backend: echo the password it received + a second line.
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("read pw; echo \"pw=$pw\"; echo second");
        let mut lines = Vec::new();
        let ok = stream_command(cmd, "s3cret", |l| lines.push(l)).unwrap();
        assert!(ok, "exit 0 should map to success");
        assert_eq!(lines, vec!["pw=s3cret".to_string(), "second".to_string()]);
    }

    #[test]
    fn stream_command_reports_failure_and_relays_stderr() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo working; echo boom 1>&2; exit 7");
        let mut lines = Vec::new();
        let ok = stream_command(cmd, "x", |l| lines.push(l)).unwrap();
        assert!(!ok, "non-zero exit should map to failure");
        assert!(lines.iter().any(|l| l.contains("working")), "stdout relayed");
        assert!(
            lines.iter().any(|l| l.contains("boom")),
            "stderr relayed on failure"
        );
    }
}
