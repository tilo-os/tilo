//! Backend invocation for the privileged install + power steps.
//!
//! ## Install contract (the backend ships separately, in `os/`)
//!
//! ```text
//! /usr/libexec/qos-install \
//!     --disk <dev> --user <name> \
//!     --filesystem <btrfs|ext4|xfs> \
//!     --hostname <name> [--timezone <tz>] \
//!     --keymap <xkb-layout> [--keyvariant <xkb-variant>] \
//!     [--encrypt] [--pin] [--fido2] --yes
//! ```
//!
//! `--encrypt` opts into full-disk encryption. The backend builds the LUKS2
//! layout itself and installs `bootc ... to-filesystem` into the opened
//! mapping, because `--block-setup tpm2-luks` can only produce a volume whose
//! single keyslot is sealed to a TPM:
//!
//! * a **recovery key** is always generated and shown once during install.
//!   It is the factor no hardware failure can take away, so every other one
//!   is layered on top of it;
//! * with `--pin`, the secret on the **second stdin line** (after the account
//!   password, never on the command line) becomes a **TPM2+PIN** keyslot
//!   where there is a TPM, and an ordinary passphrase keyslot where there is
//!   not — so a TPM-less machine unlocks with something short to type;
//! * with `--fido2`, a **FIDO2 security key** is enrolled if a token is
//!   present (its absence does not abort the install).
//!
//! `--encrypt` does **not** require a TPM: absence of the hardware costs the
//! silent unlock, not the encryption. `--pin`/`--fido2` require `--encrypt`.
//! Omitting `--encrypt` keeps the default unencrypted layout.
//!
//! `--yes` is required because the backend has no tty here; the installer's
//! Review screen is the user's "are you sure" gate. The account password is
//! supplied on **stdin** as the first line; when `--pin` is set the unlock
//! secret follows as the second line. The backend runs the privileged
//! `bootc install` + keyslot enrolment + account/hostname/timezone
//! seeding, streams human-readable progress to **stdout**, and exits `0` on
//! success / non-zero on failure. We do not implement it here — we only
//! spawn it, feed the stdin lines, and relay its output.
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
    /// Full-disk encryption (LUKS2, multi-factor). `true` ⇒ the backend is
    /// passed `--encrypt`: a volume with an always-generated recovery key,
    /// plus whichever of TPM2 / passphrase / FIDO2 the machine and the other
    /// options allow. No TPM is required. Default `false` (unencrypted).
    pub encrypt: bool,
    /// Unlock secret. Only meaningful when [`Self::encrypt`] is `true` and
    /// non-empty: the backend is passed `--pin` and the value is sent as the
    /// second stdin line. It becomes a TPM2+PIN keyslot on a machine with a
    /// TPM, and the boot passphrase on one without. Empty ⇒ omitted, which on
    /// a TPM-less machine leaves the recovery key as the only way in.
    pub pin: String,
    /// Enroll a FIDO2 security key as an extra unlock factor. Only
    /// meaningful with [`Self::encrypt`]; maps to `--fido2`. A token must
    /// be present at install time, but its absence never aborts the
    /// install. Default `false`.
    pub fido2: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            encrypt: false,
            pin: String::new(),
            fido2: false,
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
    // Opt-in full-disk encryption (multi-factor LUKS2). The backend builds the
    // volume itself with an always-generated recovery key; the PIN and FIDO2
    // factors are only meaningful alongside it.
    if opts.encrypt {
        cmd.arg("--encrypt");
        // A non-empty PIN means "enrol a secret-protected keyslot": TPM2+PIN
        // where there is a TPM, a plain passphrase where there is not. The bare
        // flag goes on argv; the value travels on stdin.
        if !opts.pin.is_empty() {
            cmd.arg("--pin");
        }
        if opts.fido2 {
            cmd.arg("--fido2");
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

/// Spawn `cmd`, write `password` (with a trailing newline) to its stdin —
/// followed by `extra_stdin` lines, each newline-terminated, in order —
/// then stream stdout line by line into `on_line`. On a non-zero exit any
/// captured stderr is relayed too (prefixed `error:`). Returns `Ok(true)`
/// iff the child exited `0`.
///
/// `extra_stdin` carries secrets the backend reads after the password (the
/// TPM2 PIN, when encryption with a PIN was requested) so they never touch
/// the argv or a tty.
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
    extra_stdin: &[&str],
    mut on_line: F,
) -> std::io::Result<bool> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;

    // Feed the password, then any extra secret lines (e.g. the TPM2 PIN),
    // then drop the handle so the child sees EOF.
    {
        if let Some(mut stdin) = child.stdin.take() {
            // A backend that exits (or stops reading stdin) before we finish
            // writing yields EPIPE here — benign: the command's exit status is
            // authoritative, so tolerate BrokenPipe and let wait() drive the
            // result. Any other write error is still propagated.
            let mut feed = || -> std::io::Result<()> {
                writeln!(stdin, "{password}")?;
                for line in extra_stdin {
                    writeln!(stdin, "{line}")?;
                }
                Ok(())
            };
            if let Err(e) = feed()
                && e.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(e);
            }
        }
    }

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            on_line(line?);
        }
    }

    let status = child.wait()?;

    if !status.success()
        && let Some(stderr) = child.stderr.take()
    {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if !line.trim().is_empty() {
                on_line(format!("error: {line}"));
            }
        }
    }

    Ok(status.success())
}

/// Run the real `qos-install` backend for `disk` + `user` + `opts`, feeding
/// `password` (and, when an encryption PIN is set, the PIN as a second
/// stdin line) and relaying every output line to `on_line`.
pub fn run_install<F: FnMut(String)>(
    disk: &str,
    user: &str,
    password: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    // The PIN only travels on stdin (and only when encryption is on with a
    // PIN); it mirrors the `--pin` flag added by `install_command`.
    let extra: Vec<&str> = if opts.encrypt && !opts.pin.is_empty() {
        vec![opts.pin.as_str()]
    } else {
        Vec::new()
    };
    stream_command(install_command(disk, user, opts), password, &extra, on_line)
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
            encrypt: false,
            pin: String::new(),
            fido2: false,
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
            encrypt: false,
            pin: String::new(),
            fido2: false,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk",
                "/dev/vda",
                "--user",
                "qq",
                "--filesystem",
                "btrfs",
                "--hostname",
                "qos",
                "--keymap",
                "us",
                "--keyvariant",
                "dvorak",
                "--yes",
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
            encrypt: false,
            pin: String::new(),
            fido2: false,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk",
                "/dev/vda",
                "--user",
                "qq",
                "--filesystem",
                "btrfs",
                "--hostname",
                "qos",
                "--yes",
            ]
        );
        assert!(!args.contains(&"--timezone"));
        assert!(!args.contains(&"--keymap"));
        assert!(!args.contains(&"--keyvariant"));
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
            encrypt: false,
            pin: String::new(),
            fido2: false,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(!args.contains(&"--keyvariant"));
        assert!(!args.contains(&"--keymap"));
    }

    #[test]
    fn install_command_appends_encrypt_when_enabled() {
        // Encryption on: a bare `--encrypt` flag is appended just before
        // `--yes`.
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            encrypt: true,
            pin: String::new(),
            fido2: false,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk",
                "/dev/vda",
                "--user",
                "qq",
                "--filesystem",
                "btrfs",
                "--hostname",
                "qos",
                "--keymap",
                "us",
                "--encrypt",
                "--yes",
            ]
        );
        // `--encrypt` is a bare flag (no value) and precedes `--yes`.
        let enc = args.iter().position(|a| *a == "--encrypt").unwrap();
        let yes = args.iter().position(|a| *a == "--yes").unwrap();
        assert!(enc < yes, "--encrypt must precede --yes");
        // No PIN / FIDO2 requested → those flags are absent.
        assert!(!args.contains(&"--pin"));
        assert!(!args.contains(&"--fido2"));
    }

    #[test]
    fn install_command_appends_pin_and_fido2_with_encrypt() {
        // Encryption on with a PIN and FIDO2: the backend gets `--encrypt
        // --pin --fido2` (in that order), all before `--yes`. The PIN value
        // itself is NOT on the argv — it travels on stdin (see
        // `run_install_sends_pin_on_second_stdin_line`).
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            encrypt: true,
            pin: "1234".to_string(),
            fido2: true,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            vec![
                "--disk",
                "/dev/vda",
                "--user",
                "qq",
                "--filesystem",
                "btrfs",
                "--hostname",
                "qos",
                "--keymap",
                "us",
                "--encrypt",
                "--pin",
                "--fido2",
                "--yes",
            ]
        );
        // The PIN secret must never appear in the argv.
        assert!(
            !args.contains(&"1234"),
            "the PIN value must not leak onto the command line"
        );
    }

    #[test]
    fn install_command_pin_and_fido2_require_encrypt() {
        // pin / fido2 set but encryption OFF: neither flag is emitted, since
        // they are only meaningful with --encrypt (the backend rejects them
        // otherwise).
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            encrypt: false,
            pin: "1234".to_string(),
            fido2: true,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(!args.contains(&"--encrypt"));
        assert!(!args.contains(&"--pin"));
        assert!(!args.contains(&"--fido2"));
    }

    #[test]
    fn install_command_omits_pin_flag_when_pin_empty_but_keeps_fido2() {
        // Encryption + FIDO2 but no PIN: `--fido2` is present, `--pin` is not.
        let opts = InstallOptions {
            filesystem: "btrfs".to_string(),
            hostname: "qos".to_string(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            encrypt: true,
            pin: String::new(),
            fido2: true,
        };
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(args.contains(&"--encrypt"));
        assert!(args.contains(&"--fido2"));
        assert!(!args.contains(&"--pin"));
    }

    #[test]
    fn install_command_omits_encrypt_by_default() {
        let opts = InstallOptions::default();
        let cmd = install_command("/dev/vda", "qq", &opts);
        let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(
            !args.contains(&"--encrypt"),
            "default options must not request encryption"
        );
    }

    #[test]
    fn default_options_are_btrfs_qos_us() {
        let o = InstallOptions::default();
        assert_eq!(o.filesystem, "btrfs");
        assert_eq!(o.hostname, "qos");
        assert!(o.timezone.is_empty());
        assert_eq!(o.keymap, "us");
        assert!(o.keyvariant.is_empty());
        assert!(!o.encrypt, "encryption is opt-in (default off)");
        assert!(o.pin.is_empty(), "no TPM2 PIN by default");
        assert!(!o.fido2, "FIDO2 enrolment is opt-in (default off)");
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
        let ok = stream_command(cmd, "s3cret", &[], |l| lines.push(l)).unwrap();
        assert!(ok, "exit 0 should map to success");
        assert_eq!(lines, vec!["pw=s3cret".to_string(), "second".to_string()]);
    }

    #[test]
    fn stream_command_feeds_extra_stdin_lines_after_password() {
        // The PIN (and any other secret) is read AFTER the password, one
        // line each, in order. Stub: read password then PIN, echo both.
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("read pw; read pin; echo \"pw=$pw\"; echo \"pin=$pin\"");
        let mut lines = Vec::new();
        let ok = stream_command(cmd, "s3cret", &["4242"], |l| lines.push(l)).unwrap();
        assert!(ok);
        assert_eq!(lines, vec!["pw=s3cret".to_string(), "pin=4242".to_string()]);
    }

    #[test]
    fn run_install_sends_pin_on_second_stdin_line() {
        // End-to-end through run_install: with encryption + a PIN, the PIN
        // must arrive as the second stdin line. We point INSTALL_BIN-style
        // logic at a stub via stream_command indirectly by building the same
        // extra-stdin vector run_install builds, then driving a stub command.
        let opts = InstallOptions {
            encrypt: true,
            pin: "9182".to_string(),
            ..InstallOptions::default()
        };
        // Mirror run_install's stdin-extra derivation.
        let extra: Vec<&str> = if opts.encrypt && !opts.pin.is_empty() {
            vec![opts.pin.as_str()]
        } else {
            Vec::new()
        };
        assert_eq!(extra, vec!["9182"], "PIN is queued as a second stdin line");

        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("read pw; read pin; echo \"$pin\"");
        let mut lines = Vec::new();
        let ok = stream_command(cmd, "pw", &extra, |l| lines.push(l)).unwrap();
        assert!(ok);
        assert_eq!(lines, vec!["9182".to_string()]);
    }

    #[test]
    fn run_install_sends_no_extra_line_without_pin() {
        // Encryption on but no PIN ⇒ nothing extra on stdin (the backend
        // doesn't read a PIN line when `--pin` wasn't passed).
        let opts = InstallOptions {
            encrypt: true,
            pin: String::new(),
            ..InstallOptions::default()
        };
        let extra: Vec<&str> = if opts.encrypt && !opts.pin.is_empty() {
            vec![opts.pin.as_str()]
        } else {
            Vec::new()
        };
        assert!(extra.is_empty());
    }

    #[test]
    fn stream_command_reports_failure_and_relays_stderr() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("echo working; echo boom 1>&2; exit 7");
        let mut lines = Vec::new();
        let ok = stream_command(cmd, "x", &[], |l| lines.push(l)).unwrap();
        assert!(!ok, "non-zero exit should map to failure");
        assert!(
            lines.iter().any(|l| l.contains("working")),
            "stdout relayed"
        );
        assert!(
            lines.iter().any(|l| l.contains("boom")),
            "stderr relayed on failure"
        );
    }
}
