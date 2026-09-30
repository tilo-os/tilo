//! Backend invocation for the privileged install + power steps.
//!
//! ## Install contract (the backend ships separately, in `os/`)
//!
//! ```text
//! /usr/libexec/tilo-install \
//!     --disk <dev> --confirm-erase <dev> --user <name> \
//!     --filesystem <btrfs|ext4|xfs> \
//!     --hostname <name> [--timezone <tz>] \
//!     --keymap <xkb-layout> [--keyvariant <xkb-variant>] \
//!     --encrypt --pin [--fido2] [--codecs] --yes
//! ```
//!
//! Native Tilo always uses full-disk encryption. The backend builds the LUKS2
//! layout itself and installs `bootc ... to-filesystem` into the opened
//! mapping, because `--block-setup tpm2-luks` can only produce a volume whose
//! single keyslot is sealed to a TPM:
//!
//! * a **recovery key** is always generated and shown once during install.
//!   It is the factor no hardware failure can take away, so every other one
//!   is layered on top of it;
//! * with `--pin` (a legacy transport flag name), the strong secret on the
//!   **second stdin line** is enrolled as a TPM2 + PIN factor when the reviewed
//!   target has an exact signed generation prediction and TPM2. Otherwise it
//!   becomes an ordinary strong LUKS passphrase; a reviewed TPM path fails
//!   closed rather than silently downgrading when those prerequisites change;
//! * with `--fido2`, a **FIDO2 security key** is enrolled if a token is
//!   present (its absence does not abort the install).
//!
//! A whole-disk install on a large enough disk also gets the room store: a
//! second LUKS2 + Btrfs partition after the root, for the storage daemon's
//! native data plane. It unlocks from a key file inside the encrypted root and
//! the recovery key opens it too, so it adds no prompt and no new factor.
//! Alongside installs get an image-backed store on first boot instead
//! (`os/usr/libexec/tilo-store-volume`).
//!
//! Encryption does **not** require a TPM. The privileged backend independently
//! enforces the mandatory encryption and strong-passphrase policy, so an older
//! or malformed UI fails closed instead of installing plaintext or weakening
//! the credential policy.
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
//! /usr/libexec/tilo-power [--reboot|--poweroff]
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
//! responsive and the status log updates live. [`stream_command`] is the
//! reusable seam shared by the backend and test stubs.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Stdio};
use zeroize::Zeroizing;

const MAX_INSTALL_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_INSTALL_OUTPUT_LINE_BYTES: usize = 64 * 1024;
const MAX_INSTALL_OUTPUT_LINES: usize = 8192;

fn read_bounded_lines(
    reader: &mut impl BufRead,
    mut on_line: impl FnMut(String),
) -> std::io::Result<()> {
    let mut pending = Vec::new();
    let mut total = 0usize;
    let mut lines = 0usize;
    loop {
        let (consumed, complete) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                break;
            }
            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .and_then(|index| index.checked_add(1))
                .unwrap_or(available.len());
            total = total.checked_add(consumed).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "installer output overflow")
            })?;
            if total > MAX_INSTALL_OUTPUT_BYTES
                || pending.len().saturating_add(consumed) > MAX_INSTALL_OUTPUT_LINE_BYTES
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "installer output exceeded its allocation bound",
                ));
            }
            let chunk = available.get(..consumed).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid output buffer")
            })?;
            pending.extend_from_slice(chunk);
            (consumed, chunk.last() == Some(&b'\n'))
        };
        reader.consume(consumed);
        if complete {
            lines = lines.checked_add(1).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "installer line overflow")
            })?;
            if lines > MAX_INSTALL_OUTPUT_LINES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "installer output exceeded its line-count bound",
                ));
            }
            while pending
                .last()
                .is_some_and(|byte| matches!(byte, b'\n' | b'\r'))
            {
                pending.pop();
            }
            on_line(String::from_utf8_lossy(&pending).into_owned());
            pending.clear();
        }
    }
    if !pending.is_empty() {
        on_line(String::from_utf8_lossy(&pending).into_owned());
    }
    Ok(())
}

use crate::disks::PartitionExtent;

/// Path to the privileged install backend installed on the live medium.
pub const INSTALL_BIN: &str = "/usr/libexec/tilo-install";
/// Path to the privileged power backend (reboot / poweroff).
pub const POWER_BIN: &str = "/usr/libexec/tilo-power";

/// User-selected install options threaded through to the backend.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallOptions {
    /// Discovery-time identity bound to the privileged target. Size and
    /// kernel device number are mandatory; WWN/serial are enforced when the
    /// hardware exposes them.
    pub disk_size_bytes: u64,
    pub disk_major_minor: String,
    pub disk_wwn: String,
    pub disk_serial: String,
    /// Empty for a whole-disk erase. Otherwise the pre-created, empty Linux
    /// partition that becomes the Tilo root while every sibling partition is
    /// preserved.
    pub alongside_root: String,
    pub alongside_root_partuuid: String,
    pub alongside_root_size_bytes: u64,
    pub alongside_boot: String,
    pub alongside_boot_partuuid: String,
    pub alongside_boot_size_bytes: u64,
    /// Existing EFI System Partition reused by an alongside install. It is
    /// mounted but never formatted.
    pub alongside_efi: String,
    pub alongside_efi_partuuid: String,
    pub alongside_efi_size_bytes: u64,
    /// Non-zero only for the guided path that appends three partitions inside a
    /// reviewed unallocated GPT extent. Existing partitions are carried as a
    /// complete identity/geometry snapshot for privileged revalidation.
    pub guided_free_start_bytes: u64,
    pub guided_free_size_bytes: u64,
    pub guided_efi_start_bytes: u64,
    pub guided_efi_size_bytes: u64,
    pub guided_boot_start_bytes: u64,
    pub guided_boot_size_bytes: u64,
    pub guided_root_start_bytes: u64,
    pub guided_root_size_bytes: u64,
    pub disk_partition_table_uuid: String,
    pub disk_logical_sector_size: u64,
    pub guided_efi_partuuid: String,
    pub guided_boot_partuuid: String,
    pub guided_root_partuuid: String,
    pub expected_partitions: Vec<PartitionExtent>,
    /// Root filesystem: `btrfs` (default), `ext4`, or `xfs`.
    pub filesystem: String,
    /// Hostname seeded into the installed system.
    pub hostname: String,
    /// Optional human-readable account name (GECOS), never a credential.
    pub full_name: String,
    /// POSIX locale seeded into `/etc/locale.conf`.
    pub locale: String,
    /// IANA time zone (e.g. `Europe/Amsterdam`). Empty ⇒ leave the
    /// backend default (UTC); the flag is then omitted.
    pub timezone: String,
    /// xkb keyboard layout (e.g. `us`, `de`). Empty ⇒ backend default
    /// (`us`); the flag is then omitted.
    pub keymap: String,
    /// Optional xkb variant (e.g. `dvorak`, `colemak`). Empty ⇒ no
    /// variant; the flag is then omitted.
    pub keyvariant: String,
    /// Mandatory startup secret, sent as the second stdin line rather than on
    /// argv. Despite the legacy field name, it is a short PIN only for the
    /// verified measured-boot TPM2 path and otherwise a strong disk passphrase.
    /// The backend independently revalidates the choice and rejects emptiness.
    pub pin: Zeroizing<String>,
    /// Enroll a FIDO2 security key as an extra unlock factor. Only
    /// meaningful alongside mandatory LUKS encryption; maps to `--fido2`. A token must
    /// be present at install time; failure to enroll an explicitly requested
    /// factor aborts the install while the temporary key still exists. Default `false`.
    pub fido2: bool,
    /// Staged (USB-free) installs only: the validated handoff copy that lets
    /// the backend target the disk holding the running installer. It is sent
    /// only with the guided plan; the backend re-derives every fact from the
    /// booted ESP, firmware and partition table and refuses any other mode.
    pub internal_handoff: String,
    /// The owner opted in to the third-party media codecs pack; maps to
    /// `--codecs`. The backend only records the choice on the installed system,
    /// which downloads the packages from their publishers once it is online.
    /// Default `false`.
    pub codecs: bool,
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            disk_size_bytes: 0,
            disk_major_minor: String::new(),
            disk_wwn: String::new(),
            disk_serial: String::new(),
            alongside_root: String::new(),
            alongside_root_partuuid: String::new(),
            alongside_root_size_bytes: 0,
            alongside_boot: String::new(),
            alongside_boot_partuuid: String::new(),
            alongside_boot_size_bytes: 0,
            alongside_efi: String::new(),
            alongside_efi_partuuid: String::new(),
            alongside_efi_size_bytes: 0,
            guided_free_start_bytes: 0,
            guided_free_size_bytes: 0,
            guided_efi_start_bytes: 0,
            guided_efi_size_bytes: 0,
            guided_boot_start_bytes: 0,
            guided_boot_size_bytes: 0,
            guided_root_start_bytes: 0,
            guided_root_size_bytes: 0,
            disk_partition_table_uuid: String::new(),
            disk_logical_sector_size: 0,
            guided_efi_partuuid: String::new(),
            guided_boot_partuuid: String::new(),
            guided_root_partuuid: String::new(),
            expected_partitions: Vec::new(),
            filesystem: "btrfs".to_string(),
            hostname: "tilo".to_string(),
            full_name: String::new(),
            locale: String::new(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            pin: Zeroizing::new(String::new()),
            fido2: false,
            internal_handoff: String::new(),
            codecs: false,
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

fn add_guided_alongside_args(cmd: &mut Command, disk: &str, opts: &InstallOptions) {
    cmd.arg("--guided-alongside")
        .arg("--free-start-bytes")
        .arg(opts.guided_free_start_bytes.to_string())
        .arg("--free-size-bytes")
        .arg(opts.guided_free_size_bytes.to_string())
        .arg("--efi-start-bytes")
        .arg(opts.guided_efi_start_bytes.to_string())
        .arg("--efi-size-bytes")
        .arg(opts.guided_efi_size_bytes.to_string())
        .arg("--boot-start-bytes")
        .arg(opts.guided_boot_start_bytes.to_string())
        .arg("--boot-size-bytes")
        .arg(opts.guided_boot_size_bytes.to_string())
        .arg("--root-start-bytes")
        .arg(opts.guided_root_start_bytes.to_string())
        .arg("--root-size-bytes")
        .arg(opts.guided_root_size_bytes.to_string())
        .arg("--expect-disk-ptuuid")
        .arg(&opts.disk_partition_table_uuid)
        .arg("--expect-logical-sector-size")
        .arg(opts.disk_logical_sector_size.to_string())
        .arg("--new-efi-partuuid")
        .arg(&opts.guided_efi_partuuid)
        .arg("--new-boot-partuuid")
        .arg(&opts.guided_boot_partuuid)
        .arg("--new-root-partuuid")
        .arg(&opts.guided_root_partuuid)
        .arg("--confirm-free-space")
        .arg(disk);
    if !opts.internal_handoff.is_empty() {
        cmd.arg("--internal-handoff").arg(&opts.internal_handoff);
    }
    for partition in &opts.expected_partitions {
        cmd.arg("--expect-partition").arg(format!(
            "{}:{}:{}",
            partition.part_uuid, partition.start_bytes, partition.size_bytes
        ));
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
    if opts.disk_size_bytes > 0 {
        cmd.arg("--expect-disk-size-bytes")
            .arg(opts.disk_size_bytes.to_string());
    }
    if !opts.disk_major_minor.is_empty() {
        cmd.arg("--expect-disk-devnum").arg(&opts.disk_major_minor);
    }
    if !opts.disk_wwn.is_empty() {
        cmd.arg("--expect-disk-wwn").arg(&opts.disk_wwn);
    }
    if !opts.disk_serial.is_empty() {
        cmd.arg("--expect-disk-serial").arg(&opts.disk_serial);
    }
    if !opts.full_name.trim().is_empty() {
        cmd.arg("--full-name").arg(opts.full_name.trim());
    }
    if !opts.locale.trim().is_empty() {
        cmd.arg("--locale").arg(opts.locale.trim());
    }
    if opts.guided_free_size_bytes > 0 {
        add_guided_alongside_args(&mut cmd, disk, opts);
    } else if opts.alongside_root.is_empty() {
        // The graphical UI has already required the person to type this exact
        // device name. Repeat that decision at the privileged boundary so an
        // older or accidentally scripted caller cannot erase with `--yes`
        // alone.
        cmd.arg("--confirm-erase").arg(disk);
    } else {
        cmd.arg("--alongside")
            .arg("--root-partition")
            .arg(&opts.alongside_root)
            .arg("--boot-partition")
            .arg(&opts.alongside_boot)
            .arg("--efi-partition")
            .arg(&opts.alongside_efi)
            .arg("--expect-root-partuuid")
            .arg(&opts.alongside_root_partuuid)
            .arg("--expect-root-size-bytes")
            .arg(opts.alongside_root_size_bytes.to_string())
            .arg("--expect-boot-partuuid")
            .arg(&opts.alongside_boot_partuuid)
            .arg("--expect-boot-size-bytes")
            .arg(opts.alongside_boot_size_bytes.to_string())
            .arg("--expect-efi-partuuid")
            .arg(&opts.alongside_efi_partuuid)
            .arg("--expect-efi-size-bytes")
            .arg(opts.alongside_efi_size_bytes.to_string())
            // Repeat the exact reviewed root at the privileged boundary. The
            // backend rejects alongside callers that omit or alter it.
            .arg("--confirm-root")
            .arg(&opts.alongside_root);
    }
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
    // Encryption is a native-Tilo invariant, not a user option. Keep the flag
    // explicit as defense in depth even though the backend defaults to LUKS.
    cmd.arg("--encrypt");
    if !opts.pin.is_empty() {
        cmd.arg("--pin");
    }
    if opts.fido2 {
        cmd.arg("--fido2");
    }
    if opts.codecs {
        cmd.arg("--codecs");
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

/// Build the backend's read-only prepared-storage preflight. The same reviewed
/// disk and partition identities are passed to the final install command; the
/// backend repeats every check before destructive work.
pub fn alongside_preflight_command(disk: &str, opts: &InstallOptions) -> Command {
    let mut cmd = Command::new(INSTALL_BIN);
    cmd.arg("--preflight-alongside")
        .arg("--disk")
        .arg(disk)
        // Satisfy install-wide input validation with fixed non-secret values;
        // preflight exits before stdin is read or account work begins.
        .arg("--user")
        .arg("preflight")
        .arg("--pin")
        .arg("--yes")
        .arg("--root-partition")
        .arg(&opts.alongside_root)
        .arg("--boot-partition")
        .arg(&opts.alongside_boot)
        .arg("--efi-partition")
        .arg(&opts.alongside_efi)
        .arg("--confirm-root")
        .arg(&opts.alongside_root)
        .arg("--expect-disk-size-bytes")
        .arg(opts.disk_size_bytes.to_string())
        .arg("--expect-disk-devnum")
        .arg(&opts.disk_major_minor)
        .arg("--expect-root-partuuid")
        .arg(&opts.alongside_root_partuuid)
        .arg("--expect-root-size-bytes")
        .arg(opts.alongside_root_size_bytes.to_string())
        .arg("--expect-boot-partuuid")
        .arg(&opts.alongside_boot_partuuid)
        .arg("--expect-boot-size-bytes")
        .arg(opts.alongside_boot_size_bytes.to_string())
        .arg("--expect-efi-partuuid")
        .arg(&opts.alongside_efi_partuuid)
        .arg("--expect-efi-size-bytes")
        .arg(opts.alongside_efi_size_bytes.to_string());
    if !opts.disk_wwn.is_empty() {
        cmd.arg("--expect-disk-wwn").arg(&opts.disk_wwn);
    }
    if !opts.disk_serial.is_empty() {
        cmd.arg("--expect-disk-serial").arg(&opts.disk_serial);
    }
    cmd
}

/// Run the read-only prepared-storage preflight and stream its diagnostics.
///
/// # Errors
///
/// Returns an error when the preflight helper cannot be started, its streams
/// cannot be read, or the process cannot be waited on. A completed preflight
/// that rejects the storage plan is returned as `Ok(false)`.
pub fn run_alongside_preflight<F: FnMut(String)>(
    disk: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    stream_command(alongside_preflight_command(disk, opts), "", &[], on_line)
}

/// Block until child `pid` has exited, leaving it unreaped.
fn wait_exited_unreaped(pid: u32) -> std::io::Result<()> {
    let pid = libc::id_t::from(pid);
    loop {
        // SAFETY: a zeroed siginfo_t is a valid out-buffer; `pid` is our
        // own unreaped child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: as above; WNOWAIT leaves the child waitable.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid,
                &raw mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Spawn `cmd`, write `password` (with a trailing newline) to its stdin —
/// followed by `extra_stdin` lines, each newline-terminated, in order —
/// then stream stdout line by line into `on_line`. On a non-zero exit any
/// captured stderr is relayed too (prefixed `error:`). Returns `Ok(true)`
/// iff the child exited `0`.
///
/// `extra_stdin` carries secrets the backend reads after the password (the
/// disk passphrase, when requested) so they never touch
/// the argv or a tty.
///
/// Separated from [`run_install`] so tests can drive it with a stub
/// command (e.g. `sh -c`) without the backend present.
///
/// # Errors
///
/// Returns `Err` if `cmd` fails to spawn, if writing `password` /
/// `extra_stdin` to its stdin fails for a reason other than a broken pipe
/// (the child exiting before it finishes reading stdin is tolerated), if a
/// stdout line can't be read, or if waiting on the child fails.
pub fn stream_command<F: FnMut(String)>(
    mut cmd: Command,
    password: &str,
    extra_stdin: &[&str],
    mut on_line: F,
) -> std::io::Result<bool> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn()?;

    // Feed the password, then any extra secret lines (the disk passphrase),
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
                bounded_command::kill_tree(&mut child);
                let _ = child.wait();
                return Err(e);
            }
        }
    }

    // Drain stderr concurrently with stdout. Helpers such as bootc, podman,
    // mkfs and cryptsetup do not share one output convention; a failure can
    // legitimately fill stderr before closing stdout. Reading the pipes in
    // sequence would then deadlock the installer and hide the actionable
    // error from the UI. Preserve the existing contract by relaying captured
    // stderr only when the command ultimately fails.
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let pid = child.id();
    // `reaped` records that the waiter collected the child: until then its
    // pid, and so its process group, cannot be reused.
    let tree = std::sync::Arc::new(std::sync::Mutex::new((child, false)));
    let stderr_reader = stderr.map(|stderr| {
        let tree = std::sync::Arc::clone(&tree);
        std::thread::spawn(move || {
            let mut lines = Vec::new();
            let read = read_bounded_lines(&mut BufReader::new(stderr), |line| {
                if !line.trim().is_empty() {
                    lines.push(line);
                }
            });
            if read.is_err() {
                // The command may still hold stdout open: stop it so the
                // stdout reader sees EOF.
                let mut tree = tree
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let (child, reaped) = &mut *tree;
                if !*reaped {
                    bounded_command::kill_tree(child);
                }
                drop(tree);
            }
            read.map(|()| lines)
        })
    });

    let stdout_result = if let Some(stdout) = stdout {
        read_bounded_lines(&mut BufReader::new(stdout), &mut on_line)
    } else {
        Ok(())
    };
    if stdout_result.is_err() {
        let mut tree = tree
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bounded_command::kill_tree(&mut tree.0);
    }
    // Wait for the exit without reaping, so the stderr reader can still stop
    // the tree meanwhile; then reap under the lock.
    let exited = wait_exited_unreaped(pid);
    let status = {
        let mut tree = tree
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (child, reaped) = &mut *tree;
        let status = exited.and_then(|()| child.wait());
        *reaped = true;
        // A backend must not daemonize children which retain the install pipes.
        // Close that escape before joining the concurrent stderr reader.
        bounded_command::kill_tree(child);
        drop(tree);
        status
    };
    let stderr_lines = stderr_reader
        .map(|reader| {
            reader
                .join()
                .map_err(|_| std::io::Error::other("stderr reader thread panicked"))
        })
        .transpose()?
        .unwrap_or(Ok(Vec::new()))?;
    stdout_result?;
    let status = status?;

    if !status.success() {
        for line in stderr_lines {
            on_line(format!("error: {line}"));
        }
    }

    Ok(status.success())
}

/// Run the `tilo-install` backend for `disk` + `user` + `opts`, feeding
/// `password` (and, when a disk startup secret is set, that secret as a second
/// stdin line) and relaying every output line to `on_line`.
///
/// # Errors
///
/// Returns `Err` under the same conditions as [`stream_command`]: the
/// backend fails to spawn, writing either secret to its stdin fails
/// (other than a broken pipe), a stdout line can't be read, or waiting on
/// the child fails.
pub fn run_install<F: FnMut(String)>(
    disk: &str,
    user: &str,
    password: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    // The startup secret only travels on stdin; it mirrors the legacy `--pin`
    // flag added by `install_command`.
    let extra: Vec<&str> = if opts.pin.is_empty() {
        Vec::new()
    } else {
        vec![opts.pin.as_str()]
    };
    stream_command(install_command(disk, user, opts), password, &extra, on_line)
}

/// Run the power backend. Returns `Ok(true)` on a clean spawn+exit — though
/// on success the machine is already on its way down, so the UI typically
/// never observes the return.
///
/// # Errors
///
/// Returns `Err` if the power backend fails to spawn or its exit status
/// can't be retrieved.
pub fn run_power(action: PowerAction) -> std::io::Result<bool> {
    let status = power_command(action).status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn install_command_binds_every_storage_identity_without_credentials() {
        // Phase `prepared_preflight_binds_every_storage_identity_without_credentials`.
        {
            let opts = InstallOptions {
                disk_size_bytes: 500_000_000_000,
                disk_major_minor: "259:0".into(),
                disk_wwn: "eui.0011223344556677".into(),
                disk_serial: "TILO-NVME".into(),
                alongside_root: "/dev/nvme0n1p3".into(),
                alongside_root_partuuid: "00000000-0000-0000-0000-000000000003".into(),
                alongside_root_size_bytes: 64_000_000_000,
                alongside_boot: "/dev/nvme0n1p5".into(),
                alongside_boot_partuuid: "00000000-0000-0000-0000-000000000005".into(),
                alongside_boot_size_bytes: 1_073_741_824,
                alongside_efi: "/dev/nvme0n1p4".into(),
                alongside_efi_partuuid: "00000000-0000-0000-0000-000000000004".into(),
                alongside_efi_size_bytes: 2_147_483_648,
                ..InstallOptions::default()
            };
            let command = alongside_preflight_command("/dev/nvme0n1", &opts);
            let args = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            for expected in [
                "--preflight-alongside",
                "/dev/nvme0n1",
                "259:0",
                "eui.0011223344556677",
                "TILO-NVME",
                "/dev/nvme0n1p3",
                "00000000-0000-0000-0000-000000000003",
                "/dev/nvme0n1p5",
                "00000000-0000-0000-0000-000000000005",
                "/dev/nvme0n1p4",
                "00000000-0000-0000-0000-000000000004",
            ] {
                assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
            }
            assert!(
                !args.iter().any(|arg| arg == "--guided-alongside"),
                "prepared preflight must never enter partition creation"
            );
        }
        // Phase `install_command_names_all_alongside_partitions`.
        {
            let opts = InstallOptions {
                disk_size_bytes: 1_000_000_000_000,
                disk_major_minor: "259:0".into(),
                disk_wwn: "eui.0011223344556677".into(),
                disk_serial: "TILO-NVME".into(),
                alongside_root: "/dev/nvme0n1p4".into(),
                alongside_root_partuuid: "00000000-0000-0000-0000-000000000004".into(),
                alongside_root_size_bytes: 68_719_476_736,
                alongside_boot: "/dev/nvme0n1p5".into(),
                alongside_boot_partuuid: "00000000-0000-0000-0000-000000000005".into(),
                alongside_boot_size_bytes: 1_073_741_824,
                alongside_efi: "/dev/nvme0n1p1".into(),
                alongside_efi_partuuid: "00000000-0000-0000-0000-000000000001".into(),
                alongside_efi_size_bytes: 2_147_483_648,
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--root-partition", "/dev/nvme0n1p4"])
            );
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--efi-partition", "/dev/nvme0n1p1"])
            );
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--boot-partition", "/dev/nvme0n1p5"])
            );
            assert!(args.contains(&"--alongside"));
            for expected in [
                ["--expect-disk-size-bytes", "1000000000000"],
                ["--expect-disk-devnum", "259:0"],
                ["--expect-disk-wwn", "eui.0011223344556677"],
                ["--expect-disk-serial", "TILO-NVME"],
                [
                    "--expect-root-partuuid",
                    "00000000-0000-0000-0000-000000000004",
                ],
                ["--expect-root-size-bytes", "68719476736"],
                [
                    "--expect-efi-partuuid",
                    "00000000-0000-0000-0000-000000000001",
                ],
                ["--expect-efi-size-bytes", "2147483648"],
            ] {
                assert!(
                    args.windows(2).any(|pair| pair == expected),
                    "missing identity binding {expected:?} in {args:?}"
                );
            }
            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--confirm-root", "/dev/nvme0n1p4"])
            );
            assert!(!args.contains(&"--confirm-erase"));
        }
        // Phase `install_command_binds_the_complete_guided_free_space_plan`.
        {
            let opts = InstallOptions {
                disk_size_bytes: 200 * 1024 * 1024 * 1024,
                disk_major_minor: "259:0".into(),
                disk_serial: "TILO-NVME".into(),
                guided_free_start_bytes: 100 * 1024 * 1024 * 1024,
                guided_free_size_bytes: 100 * 1024 * 1024 * 1024 - 1024 * 1024,
                guided_efi_start_bytes: 100 * 1024 * 1024 * 1024,
                guided_efi_size_bytes: 2 * 1024 * 1024 * 1024,
                guided_boot_start_bytes: 102 * 1024 * 1024 * 1024,
                guided_boot_size_bytes: 1024 * 1024 * 1024,
                guided_root_start_bytes: 103 * 1024 * 1024 * 1024,
                guided_root_size_bytes: 97 * 1024 * 1024 * 1024 - 1024 * 1024,
                disk_partition_table_uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
                disk_logical_sector_size: 512,
                guided_efi_partuuid: "10000000-0000-4000-8000-000000000001".into(),
                guided_boot_partuuid: "30000000-0000-4000-8000-000000000003".into(),
                guided_root_partuuid: "20000000-0000-4000-8000-000000000002".into(),
                expected_partitions: vec![PartitionExtent {
                    part_uuid: "00000000-0000-0000-0000-000000000001".into(),
                    start_bytes: 1024 * 1024,
                    size_bytes: 100 * 1024 * 1024 * 1024 - 1024 * 1024,
                }],
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|arg| arg.to_str().unwrap()).collect();
            assert!(args.contains(&"--guided-alongside"));
            assert!(!args.contains(&"--confirm-erase"));
            for expected in [
                [
                    "--expect-disk-ptuuid",
                    "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
                ],
                ["--expect-logical-sector-size", "512"],
                ["--confirm-free-space", "/dev/nvme0n1"],
                [
                    "--expect-partition",
                    "00000000-0000-0000-0000-000000000001:1048576:107373133824",
                ],
            ] {
                assert!(
                    args.windows(2).any(|pair| pair == expected),
                    "missing guided binding {expected:?}: {args:?}"
                );
            }
        }
        // Phase `internal_handoff_travels_only_with_the_guided_plan`.
        {
            let guided = InstallOptions {
                guided_free_start_bytes: 100 * 1024 * 1024 * 1024,
                guided_free_size_bytes: 64 * 1024 * 1024 * 1024,
                internal_handoff: crate::internal_handoff::HANDOFF_PATH.into(),
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &guided);
            let args: Vec<&str> = cmd.get_args().map(|arg| arg.to_str().unwrap()).collect();
            assert!(
                args.windows(2)
                    .any(|pair| pair
                        == ["--internal-handoff", crate::internal_handoff::HANDOFF_PATH])
            );
            assert!(args.contains(&"--guided-alongside"));

            // A whole-disk or prepared command never carries it, so the backend's
            // live-disk refusal stays in force for those modes.
            let erase = InstallOptions {
                internal_handoff: crate::internal_handoff::HANDOFF_PATH.into(),
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &erase);
            assert!(!cmd.get_args().any(|arg| arg == "--internal-handoff"));
        }
        // Phase `whole_disk_command_binds_all_available_discovery_identity`.
        {
            let opts = InstallOptions {
                disk_size_bytes: 64 * 1024 * 1024 * 1024,
                disk_major_minor: "252:0".into(),
                disk_wwn: "0x5000c500aabbccdd".into(),
                disk_serial: "VM-DISK-01".into(),
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/vda", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            for expected in [
                ["--expect-disk-size-bytes", "68719476736"],
                ["--expect-disk-devnum", "252:0"],
                ["--expect-disk-wwn", "0x5000c500aabbccdd"],
                ["--expect-disk-serial", "VM-DISK-01"],
            ] {
                assert!(args.windows(2).any(|pair| pair == expected));
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn install_and_power_commands_build_argv_from_options() {
        // Phase `install_command_builds_expected_argv`.
        {
            let opts = InstallOptions {
                filesystem: "ext4".to_string(),
                hostname: "lab-1".to_string(),
                full_name: "ada Example".to_string(),
                locale: "nl_NL.UTF-8".to_string(),
                timezone: "Europe/Amsterdam".to_string(),
                keymap: "de".to_string(),
                keyvariant: String::new(),
                pin: String::new().into(),
                fido2: false,
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/sda", "ada", &opts);
            assert_eq!(cmd.get_program(), INSTALL_BIN);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            assert_eq!(
                args,
                vec![
                    "--disk",
                    "/dev/sda",
                    "--user",
                    "ada",
                    "--filesystem",
                    "ext4",
                    "--hostname",
                    "lab-1",
                    "--full-name",
                    "ada Example",
                    "--locale",
                    "nl_NL.UTF-8",
                    "--confirm-erase",
                    "/dev/sda",
                    "--timezone",
                    "Europe/Amsterdam",
                    "--keymap",
                    "de",
                    "--encrypt",
                    "--yes",
                ]
            );
        }
        // Phase `install_command_passes_keyvariant_with_keymap`.
        {
            // Dvorak = xkb layout "us" + variant "dvorak".
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: "us".to_string(),
                keyvariant: "dvorak".to_string(),
                pin: String::new().into(),
                fido2: false,
                ..InstallOptions::default()
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
                    "tilo",
                    "--confirm-erase",
                    "/dev/vda",
                    "--keymap",
                    "us",
                    "--keyvariant",
                    "dvorak",
                    "--encrypt",
                    "--yes",
                ]
            );
        }
        // Phase `install_command_omits_empty_timezone_and_keymap`.
        {
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: String::new(),
                keyvariant: String::new(),
                pin: String::new().into(),
                fido2: false,
                ..InstallOptions::default()
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
                    "tilo",
                    "--confirm-erase",
                    "/dev/vda",
                    "--encrypt",
                    "--yes",
                ]
            );
            assert!(!args.contains(&"--timezone"));
            assert!(!args.contains(&"--keymap"));
            assert!(!args.contains(&"--keyvariant"));
        }
        // Phase `install_command_omits_keyvariant_when_no_keymap`.
        {
            // A stray variant without a layout must not be emitted alone.
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: String::new(),
                keyvariant: "dvorak".to_string(),
                pin: String::new().into(),
                fido2: false,
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/vda", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            assert!(!args.contains(&"--keyvariant"));
            assert!(!args.contains(&"--keymap"));
        }
        // Phase `default_options_are_btrfs_tilo_us`.
        {
            let o = InstallOptions::default();
            assert_eq!(o.filesystem, "btrfs");
            assert_eq!(o.hostname, "tilo");
            assert!(o.timezone.is_empty());
            assert_eq!(o.keymap, "us");
            assert!(o.keyvariant.is_empty());
            assert!(o.pin.is_empty(), "no TPM2 PIN by default");
            assert!(!o.fido2, "FIDO2 enrolment is opt-in (default off)");
            assert!(!o.codecs, "media codecs are opt-in (default off)");
        }
        // Phase `power_command_builds_expected_argv`.
        {
            let reboot = power_command(PowerAction::Reboot);
            assert_eq!(reboot.get_program(), POWER_BIN);
            let args: Vec<&str> = reboot.get_args().map(|a| a.to_str().unwrap()).collect();
            assert_eq!(args, vec!["--reboot"]);

            let off = power_command(PowerAction::PowerOff);
            let args: Vec<&str> = off.get_args().map(|a| a.to_str().unwrap()).collect();
            assert_eq!(args, vec!["--poweroff"]);
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn install_command_always_encrypts_with_optional_factors() {
        // Phase `install_command_always_appends_encrypt`.
        {
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: "us".to_string(),
                keyvariant: String::new(),
                pin: String::new().into(),
                fido2: false,
                ..InstallOptions::default()
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
                    "tilo",
                    "--confirm-erase",
                    "/dev/vda",
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
        // Phase `install_command_appends_pin_and_fido2_with_encrypt`.
        {
            // Encryption on with a PIN and FIDO2: the backend gets `--encrypt
            // --pin --fido2` (in that order), all before `--yes`. The PIN value
            // itself is NOT on the argv — it travels on stdin (see
            // `stream_command_feeds_secrets_on_stdin_and_bounds_output`).
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: "us".to_string(),
                keyvariant: String::new(),
                pin: "1234".to_string().into(),
                fido2: true,
                ..InstallOptions::default()
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
                    "tilo",
                    "--confirm-erase",
                    "/dev/vda",
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
        // Phase `install_command_keeps_encryption_with_additional_factors`.
        {
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: "us".to_string(),
                keyvariant: String::new(),
                pin: "1234".to_string().into(),
                fido2: true,
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/vda", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            assert!(args.contains(&"--encrypt"));
            assert!(args.contains(&"--pin"));
            assert!(args.contains(&"--fido2"));
        }
        // Phase `install_command_omits_pin_flag_when_pin_empty_but_keeps_fido2`.
        {
            // Encryption + FIDO2 but no PIN: `--fido2` is present, `--pin` is not.
            let opts = InstallOptions {
                filesystem: "btrfs".to_string(),
                hostname: "tilo".to_string(),
                timezone: String::new(),
                keymap: "us".to_string(),
                keyvariant: String::new(),
                pin: String::new().into(),
                fido2: true,
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/vda", "qq", &opts);
            let args: Vec<&str> = cmd.get_args().map(|a| a.to_str().unwrap()).collect();
            assert!(args.contains(&"--encrypt"));
            assert!(args.contains(&"--fido2"));
            assert!(!args.contains(&"--pin"));
        }
        // Phase `install_command_encrypts_by_default`.
        {
            let opts = InstallOptions::default();
            let cmd = install_command("/dev/vda", "qq", &opts);
            let mut args = cmd.get_args().map(|a| a.to_str().unwrap());
            assert!(args.any(|a| a == "--encrypt"));
        }
    }

    #[test]
    fn install_command_passes_the_codecs_opt_in_only_when_chosen() {
        let chosen = InstallOptions {
            codecs: true,
            ..InstallOptions::default()
        };
        let args: Vec<String> = install_command("/dev/vda", "qq", &chosen)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let codecs = args.iter().position(|a| a == "--codecs").expect("--codecs");
        let yes = args.iter().position(|a| a == "--yes").expect("--yes");
        assert!(codecs < yes, "flags come before --yes");
        let default: Vec<String> = install_command("/dev/vda", "qq", &InstallOptions::default())
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!default.iter().any(|a| a == "--codecs"));
    }

    #[test]
    fn stream_command_feeds_secrets_on_stdin_and_bounds_output() {
        // Phase `stream_command_reads_password_and_collects_stdout`.
        {
            // Stub backend: echo the password it received + a second line.
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("read pw; echo \"pw=$pw\"; echo second");
            let mut lines = Vec::new();
            let ok = stream_command(cmd, "s3cret", &[], |l| lines.push(l)).unwrap();
            assert!(ok, "exit 0 should map to success");
            assert_eq!(lines, vec!["pw=s3cret".to_string(), "second".to_string()]);
        }
        // Phase `stream_command_feeds_extra_stdin_lines_after_password`.
        {
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
        // Phase `run_install_sends_pin_on_second_stdin_line`.
        {
            // End-to-end through run_install: with encryption + a PIN, the PIN
            // must arrive as the second stdin line. We point INSTALL_BIN-style
            // logic at a stub via stream_command indirectly by building the same
            // extra-stdin vector run_install builds, then driving a stub command.
            let opts = InstallOptions {
                pin: "9182".to_string().into(),
                ..InstallOptions::default()
            };
            // Mirror run_install's stdin-extra derivation.
            let extra: Vec<&str> = if opts.pin.is_empty() {
                Vec::new()
            } else {
                vec![opts.pin.as_str()]
            };
            assert_eq!(extra, vec!["9182"], "PIN is queued as a second stdin line");

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("read pw; read pin; echo \"$pin\"");
            let mut lines = Vec::new();
            let ok = stream_command(cmd, "pw", &extra, |l| lines.push(l)).unwrap();
            assert!(ok);
            assert_eq!(lines, vec!["9182".to_string()]);
        }
        // Phase `run_install_sends_no_extra_line_without_pin`.
        {
            // Encryption on but no PIN ⇒ nothing extra on stdin (the backend
            // doesn't read a PIN line when `--pin` wasn't passed).
            let opts = InstallOptions {
                pin: String::new().into(),
                ..InstallOptions::default()
            };
            let extra: Vec<&str> = if opts.pin.is_empty() {
                Vec::new()
            } else {
                vec![opts.pin.as_str()]
            };
            assert!(extra.is_empty());
        }
        // Phase `stream_command_reports_failure_and_relays_stderr`.
        {
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
        // Phase `stream_command_drains_large_stderr_without_deadlocking`.
        {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(
                "i=0; while [ \"$i\" -lt 4096 ]; do \
                 echo \"installer diagnostic $i fills the error pipe\" >&2; \
                 i=$((i + 1)); done; echo reached-stdout; exit 9",
            );
            let mut lines = Vec::new();
            let ok = stream_command(cmd, "x", &[], |line| lines.push(line)).unwrap();
            assert!(!ok);
            assert!(lines.iter().any(|line| line == "reached-stdout"));
            assert!(
                lines
                    .iter()
                    .any(|line| line == "error: installer diagnostic 4095 fills the error pipe"),
                "the tail of stderr was drained and relayed"
            );
        }
        // Phase `stream_command_rejects_oversized_output_on_either_stream`.
        {
            // The stdout producer then stays alive holding its pipe: the call
            // returns only because the overflow kills the command's tree.
            // Likewise the stderr producers, whether they keep stdout open or
            // closed it first: the call returns only if the stderr overflow
            // kills the tree too.
            for script in [
                "head -c 70000 /dev/zero; exec sleep 3600",
                "head -c 70000 /dev/zero >&2; exec sleep 3600",
                "exec >&-; head -c 70000 /dev/zero >&2; exec sleep 3600",
            ] {
                let mut cmd = Command::new("sh");
                cmd.arg("-c").arg(script);
                let (done, result) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = done.send(stream_command(cmd, "x", &[], |_| {}));
                });
                let error = result
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap_or_else(|_| panic!("an overflowing command was left running: {script}"))
                    .unwrap_err();
                assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            }
        }
    }
}
