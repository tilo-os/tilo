//! Runs the privileged installer and power helpers, which ship separately in `os/`.
//!
//! ```text
//! /usr/libexec/tilo-install --disk <dev> --confirm-erase <dev> --user <name> \
//!     --filesystem <fs> --hostname <name> [--timezone <tz>] [--keymap <layout>] \
//!     [--keyvariant <variant>] --encrypt --pin [--fido2] [--extension media-codecs] --yes
//! ```
//!
//! `--yes` is required because there is no tty. The account password arrives as stdin's first
//! line, the disk secret as the second when `--pin` is set, so neither appears on argv.

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

use crate::destination::disks::PartitionExtent;
use tilo_lock::MutexRecover as _;

pub const INSTALL_BIN: &str = "/usr/libexec/tilo-install";
pub const POWER_BIN: &str = "/usr/libexec/tilo-power";

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is an independent backend policy flag, not a state combination"
)]
#[derive(Clone, PartialEq, Eq)]
pub struct InstallOptions {
    /// Size and kernel device number are mandatory; WWN and serial are checked when exposed.
    pub disk_size_bytes: u64,
    pub disk_major_minor: String,
    pub disk_wwn: String,
    pub disk_serial: String,
    /// Empty for a whole-disk erase; otherwise the empty partition that becomes the Tilo root.
    pub alongside_root: String,
    pub alongside_root_partuuid: String,
    pub alongside_root_size_bytes: u64,
    pub alongside_boot: String,
    pub alongside_boot_partuuid: String,
    pub alongside_boot_size_bytes: u64,
    /// Mounted, never formatted.
    pub alongside_efi: String,
    pub alongside_efi_partuuid: String,
    pub alongside_efi_size_bytes: u64,
    /// Non-zero only for the guided path, which appends three partitions in reviewed free space.
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
    pub purpose: String,
    pub device_label: String,
    pub prepared_setup: Option<install_profile_model::PreparedSetupExpectation>,
    /// Public TPM-sealed sidecar for the prepared package above.
    pub tpm_handoff: Option<String>,
    pub hostname: String,
    /// Account name (GECOS); never a credential.
    pub full_name: String,
    /// The first stdin line is already a SHA-512 crypt hash, not a password. Only the unattended
    /// recipe path sets this.
    pub password_is_hash: bool,
    /// Enrol the secret as a plain strong LUKS passphrase instead of the measured-boot TPM2 PIN
    /// policy; the interactive flow leaves the reviewed target's choice alone.
    pub force_startup_passphrase: bool,
    /// POSIX locale seeded into `/etc/locale.conf`.
    pub locale: String,
    /// IANA time zone; empty omits the flag.
    pub timezone: String,
    /// xkb layout; empty omits the flag and the backend uses `us`.
    pub keymap: String,
    pub keyvariant: String,
    /// The disk secret, sent on stdin and never on argv: a short PIN only on the measured-boot
    /// TPM2 path. The backend rejects an empty one.
    pub pin: Zeroizing<String>,
    /// Enrol a FIDO2 key as an extra unlock factor; the token must be present, and failing to
    /// enrol one that was asked for aborts the install.
    pub fido2: bool,
    /// Staged (USB-free) installs only: the validated handoff copy names the installer's disk.
    pub internal_handoff: String,
    /// Install the media codecs extension, which downloads the formats from their publishers.
    pub codecs: bool,
    /// Never serialized into a prepared profile.
    pub online_accounts:
        Option<std::sync::Arc<tilo_install_helpers::account_handoff::InstallAccounts>>,
    /// Identity of the operation confirmed on Review.
    pub account_operation: String,
}

pub const MEDIA_CODECS_EXTENSION: &str = "media-codecs";

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
            purpose: install_profile_model::default_purpose(),
            device_label: String::new(),
            prepared_setup: None,
            tpm_handoff: None,
            filesystem: "btrfs".to_string(),
            hostname: "tilo".to_string(),
            full_name: String::new(),
            password_is_hash: false,
            force_startup_passphrase: false,
            locale: String::new(),
            timezone: String::new(),
            keymap: "us".to_string(),
            keyvariant: String::new(),
            pin: Zeroizing::new(String::new()),
            fido2: false,
            internal_handoff: String::new(),
            codecs: false,
            online_accounts: None,
            account_operation: String::new(),
        }
    }
}

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

/// Builds the backend command; [`stream_command`] wires stdin and output.
#[expect(
    clippy::too_many_lines,
    reason = "the argv binding is one ordered contract and is easier to audit in place"
)]
pub fn install_command(disk: &str, user: &str, opts: &InstallOptions) -> Command {
    let mut cmd = Command::new(INSTALL_BIN);
    if opts.online_accounts.is_some() {
        cmd.arg("--account-operation").arg(&opts.account_operation);
    }
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
    if opts.password_is_hash {
        cmd.arg("--password-hash");
    }
    if opts.force_startup_passphrase {
        cmd.arg("--startup-passphrase-only");
    }
    if !opts.locale.trim().is_empty() {
        cmd.arg("--locale").arg(opts.locale.trim());
    }
    if opts.guided_free_size_bytes > 0 {
        add_guided_alongside_args(&mut cmd, disk, opts);
    } else if opts.alongside_root.is_empty() {
        // The UI asked for this exact device name; repeat it so `--yes` alone cannot erase.
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
            // The backend rejects alongside callers that omit or alter the reviewed root.
            .arg("--confirm-root")
            .arg(&opts.alongside_root);
    }
    if !opts.device_label.is_empty() {
        cmd.arg("--device-label").arg(&opts.device_label);
    }
    if let Some(expected) = &opts.prepared_setup {
        cmd.arg("--prepared-setup-receipt").arg(
            serde_json::json!({
                "schema": expected.schema,
                "sealed_bytes": expected.sealed_bytes,
                "ciphertext_sha256": expected.ciphertext_sha256,
            })
            .to_string(),
        );
    }
    if let Some(handoff) = &opts.tpm_handoff {
        cmd.arg("--prepared-tpm-handoff").arg(handoff);
    }
    if opts.purpose != "computer" {
        cmd.arg("--purpose").arg(&opts.purpose);
    }
    if !opts.timezone.trim().is_empty() {
        cmd.arg("--timezone").arg(opts.timezone.trim());
    }
    if !opts.keymap.trim().is_empty() {
        cmd.arg("--keymap").arg(opts.keymap.trim());
        if !opts.keyvariant.trim().is_empty() {
            cmd.arg("--keyvariant").arg(opts.keyvariant.trim());
        }
    }
    // Encryption is not a user option; pass the flag even though the backend defaults to LUKS.
    cmd.arg("--encrypt");
    if !opts.pin.is_empty() {
        cmd.arg("--pin");
    }
    if opts.fido2 {
        cmd.arg("--fido2");
    }
    if opts.codecs {
        cmd.args(["--extension", MEDIA_CODECS_EXTENSION]);
    }
    cmd.arg("--yes");
    cmd
}

pub fn power_command(action: PowerAction) -> Command {
    let mut cmd = Command::new(POWER_BIN);
    cmd.arg(action.flag());
    cmd
}

/// The read-only storage preflight, with the same disk and partition identities as the install
/// command; the backend repeats every check before destructive work.
pub fn alongside_preflight_command(disk: &str, opts: &InstallOptions) -> Command {
    let mut cmd = Command::new(INSTALL_BIN);
    cmd.arg("--preflight-alongside")
        .arg("--disk")
        .arg(disk)
        // Fixed non-secret values: preflight exits before stdin is read.
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

/// Runs the read-only preflight and streams its diagnostics. Returns `Ok(false)` when it
/// rejects the storage plan.
pub fn run_alongside_preflight<F: FnMut(String)>(
    disk: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    stream_command(alongside_preflight_command(disk, opts), "", &[], on_line)
}

/// Blocks until child `pid` has exited, without reaping it.
fn wait_exited_unreaped(pid: u32) -> std::io::Result<()> {
    let pid = libc::id_t::from(pid);
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid out-buffer; WNOWAIT leaves the child waitable.
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

/// Spawns `cmd`, writes `password` and then each of `extra_stdin` as newline-terminated stdin
/// lines, and relays stdout line by line to `on_line`; on failure captured stderr follows.
/// Returns whether the child exited `0`. A child that exits before reading stdin is tolerated.
pub fn stream_command<F: FnMut(String)>(
    cmd: Command,
    password: &str,
    extra_stdin: &[&str],
    on_line: F,
) -> std::io::Result<bool> {
    stream_command_with_accounts(cmd, password, extra_stdin, None, on_line)
}

fn write_install_input(
    stdin: Option<std::process::ChildStdin>,
    password: &str,
    extra: &[String],
    accounts: Option<&tilo_install_helpers::account_handoff::InstallAccounts>,
) -> std::io::Result<()> {
    if let Some(mut stdin) = stdin {
        writeln!(stdin, "{password}")?;
        for line in extra {
            writeln!(stdin, "{line}")?;
        }
        if let Some(accounts) = accounts {
            accounts.write_to(&mut stdin)?;
        }
    }
    Ok(())
}

fn stream_command_with_accounts<F: FnMut(String)>(
    mut cmd: Command,
    password: &str,
    extra_stdin: &[&str],
    accounts: Option<std::sync::Arc<tilo_install_helpers::account_handoff::InstallAccounts>>,
    mut on_line: F,
) -> std::io::Result<bool> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = cmd.spawn()?;

    let stdin = child.stdin.take();
    let password = Zeroizing::new(password.to_owned());
    let extra_stdin = Zeroizing::new(
        extra_stdin
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>(),
    );

    // Read stderr too: bootc, mkfs and cryptsetup may fill it before closing stdout, so reading
    // in sequence would deadlock.
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let pid = child.id();
    // Until reaped, the child's pid and process group cannot be reused.
    let tree = std::sync::Arc::new(std::sync::Mutex::new((child, false)));
    // The disk secret can exceed pipe capacity; feed it while draining output.
    let writer_tree = std::sync::Arc::clone(&tree);
    let stdin_writer = std::thread::spawn(move || -> std::io::Result<()> {
        let result = write_install_input(stdin, &password, &extra_stdin, accounts.as_deref());
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
            Err(error) => {
                let mut tree = writer_tree
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !tree.1 {
                    bounded_command::kill_tree(&mut tree.0);
                }
                drop(tree);
                Err(error)
            }
            Ok(()) => Ok(()),
        }
    });
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
                // The command may still hold stdout: stop it so the reader sees EOF.
                let mut tree = tree.lock_recover();
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
        let mut tree = tree.lock_recover();
        bounded_command::kill_tree(&mut tree.0);
    }
    // Wait without reaping so the stderr reader can still stop the tree; then reap under the lock.
    let exited = wait_exited_unreaped(pid);
    let status = {
        let mut tree = tree.lock_recover();
        let (child, reaped) = &mut *tree;
        let status = exited.and_then(|()| child.wait());
        *reaped = true;
        // Close the way for a backend to leave children holding the pipes, before joining the
        // stderr reader.
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
    stdin_writer
        .join()
        .map_err(|_| std::io::Error::other("stdin writer thread panicked"))??;
    stdout_result?;
    let status = status?;

    if !status.success() {
        for line in stderr_lines {
            on_line(format!("error: {line}"));
        }
    }

    Ok(status.success())
}

/// Runs the `tilo-install` backend, feeding the password and, when a disk secret is set, that
/// secret as the next stdin line, and relaying every output line to `on_line`.
pub fn run_install<F: FnMut(String)>(
    disk: &str,
    user: &str,
    password: &str,
    opts: &InstallOptions,
    on_line: F,
) -> std::io::Result<bool> {
    // The disk secret travels on stdin, matching the `--pin` flag.
    let extra: Vec<&str> = if opts.pin.is_empty() {
        Vec::new()
    } else {
        vec![opts.pin.as_str()]
    };
    if opts.online_accounts.is_some()
        && (uuid::Uuid::parse_str(&opts.account_operation).is_err() || opts.pin.is_empty())
    {
        return Err(std::io::Error::other("invalid selected-account operation"));
    }
    if opts.password_is_hash {
        if password.is_empty() {
            return Err(std::io::Error::other("missing account password hash"));
        }
        if opts.prepared_setup.is_some() || opts.online_accounts.is_some() {
            return Err(std::io::Error::other(
                "a prepared setup or account transport cannot take a password hash",
            ));
        }
    }
    stream_command_with_accounts(
        install_command(disk, user, opts),
        password,
        &extra,
        opts.online_accounts.clone(),
        on_line,
    )
}

/// Runs the power backend. On success the machine is already going down, so the UI rarely sees
/// the return.
pub fn run_power(action: PowerAction) -> std::io::Result<bool> {
    let status = power_command(action).status()?;
    Ok(status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_transport_drains_progress_while_feeding_private_frame() {
        use account_custody::portable::{self, PortableBinding};
        use accounts::{Account, AccountId, AccountStore, Secret, SecretVault};
        use sha2::{Digest, Sha256};
        let source = tempfile::tempdir().unwrap();
        let account = Account::new(AccountId::new("selected"), "webdav");
        let mut vault = SecretVault::open_in(source.path());
        vault.put(&account.id, &Secret::password("s".repeat(256 * 1024)));
        vault.try_save().unwrap();
        let mut store = AccountStore::new();
        store.upsert(account.clone());
        store.save_in(source.path()).unwrap();
        let key = tilo_seal::SymmetricKey::generate().unwrap();
        let operation = uuid::Uuid::new_v4().to_string();
        let binding = PortableBinding::new(&operation, "disk", "person", 1000).unwrap();
        let sealed = portable::seal_selected_accounts(
            source.path(),
            &[account.id],
            &key,
            binding.as_bytes(),
        )
        .unwrap();
        let accounts = std::sync::Arc::new(
            tilo_install_helpers::account_handoff::InstallAccounts::new(&sealed, key),
        );
        let mut frame = Zeroizing::new(Vec::new());
        accounts.write_to(&mut *frame).unwrap();
        let digest =
            Sha256::digest(&*frame)
                .iter()
                .fold(String::with_capacity(64), |mut output, byte| {
                    use std::fmt::Write as _;
                    let _ = write!(output, "{byte:02x}");
                    output
                });
        let expected = format!("{digest}  -");
        let opts = InstallOptions {
            online_accounts: Some(accounts.clone()),
            account_operation: operation.clone(),
            ..InstallOptions::default()
        };
        let command = install_command("/dev/fixture", "person", &opts);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--account-operation", operation.as_str()])
        );
        assert!(
            !args
                .iter()
                .any(|arg| arg.contains("selected") || arg.contains("ssss"))
        );
        // A sequential writer deadlocks once progress fills stdout before the backend reads a
        // selection larger than the pipe.
        let mut command = Command::new("timeout");
        command.args([
            "10s",
            "bash",
            "-c",
            r#"
            IFS= read -r password; IFS= read -r pin
            [ "$password" = login ] && [ "$pin" = 123456 ] || exit 2
            for ((i=0; i<2048; i++)); do printf '%128s\n' progress; done
            sha256sum
        "#,
        ]);
        let mut last = String::new();
        assert!(
            stream_command_with_accounts(command, "login", &["123456"], Some(accounts), |line| {
                last = line;
            },)
            .unwrap()
        );
        assert_eq!(last, expected);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "scenario test: each labelled phase keeps one former test's fixture and assertions"
    )]
    fn install_command_binds_every_storage_identity_without_credentials() {
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
        {
            let guided = InstallOptions {
                guided_free_start_bytes: 100 * 1024 * 1024 * 1024,
                guided_free_size_bytes: 64 * 1024 * 1024 * 1024,
                internal_handoff: crate::destination::internal_handoff::HANDOFF_PATH.into(),
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &guided);
            let args: Vec<&str> = cmd.get_args().map(|arg| arg.to_str().unwrap()).collect();
            assert!(args.windows(2).any(|pair| pair
                == [
                    "--internal-handoff",
                    crate::destination::internal_handoff::HANDOFF_PATH
                ]));
            assert!(args.contains(&"--guided-alongside"));

            // A whole-disk command carries no handoff, so the live-disk refusal stays in force.
            let erase = InstallOptions {
                internal_handoff: crate::destination::internal_handoff::HANDOFF_PATH.into(),
                ..InstallOptions::default()
            };
            let cmd = install_command("/dev/nvme0n1", "qq", &erase);
            assert!(!cmd.get_args().any(|arg| arg == "--internal-handoff"));
        }
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
        {
            let opts = InstallOptions {
                filesystem: "ext4".to_string(),
                hostname: "lab-1".to_string(),
                full_name: "Ada Example".to_string(),
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
                    "Ada Example",
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
        {
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
        {
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
            let enc = args.iter().position(|a| *a == "--encrypt").unwrap();
            let yes = args.iter().position(|a| *a == "--yes").unwrap();
            assert!(enc < yes, "--encrypt must precede --yes");
            assert!(!args.contains(&"--pin"));
            assert!(!args.contains(&"--fido2"));
        }
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
            assert!(
                !args.contains(&"1234"),
                "the PIN value must not leak onto the command line"
            );
        }
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
        {
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
        {
            let opts = InstallOptions::default();
            let cmd = install_command("/dev/vda", "qq", &opts);
            let mut args = cmd.get_args().map(|a| a.to_str().unwrap());
            assert!(args.any(|a| a == "--encrypt"));
        }
    }

    #[test]
    fn install_command_preserves_the_reviewed_ciphertext_binding_and_label() {
        let expected = install_profile_model::PreparedSetupExpectation {
            schema: 1,
            sealed_bytes: 4096,
            ciphertext_sha256: "a".repeat(64),
        };
        let command = install_command(
            "/dev/test",
            "owner",
            &InstallOptions {
                purpose: "server".into(),
                device_label: "Travel computer".into(),
                prepared_setup: Some(expected.clone()),
                ..Default::default()
            },
        );
        let args: Vec<_> = command.get_args().collect();
        let receipt = args
            .windows(2)
            .find(|pair| pair[0] == "--prepared-setup-receipt")
            .unwrap();
        let decoded: install_profile_model::PreparedSetupExpectation =
            serde_json::from_str(receipt[1].to_str().unwrap()).unwrap();
        assert_eq!(decoded, expected);
        let label = args
            .windows(2)
            .find(|pair| pair[0] == "--device-label")
            .unwrap();
        assert_eq!(label[1], "Travel computer");
    }

    #[test]
    fn install_command_carries_purpose_without_selecting_another_image() {
        let computer = install_command("/dev/test", "ada", &InstallOptions::default());
        let server = install_command(
            "/dev/test",
            "ada",
            &InstallOptions {
                purpose: "server".into(),
                ..Default::default()
            },
        );
        let server_args: Vec<_> = server.get_args().collect();
        assert!(!computer.get_args().any(|arg| arg == "--purpose"));
        let purpose = server_args
            .windows(2)
            .find(|pair| pair[0] == "--purpose")
            .unwrap();
        assert_eq!(purpose[1], "server");
        assert_eq!(server.get_program(), computer.get_program());
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
        let codecs = args
            .iter()
            .position(|a| a == "--extension")
            .expect("--extension");
        assert_eq!(args[codecs + 1], "media-codecs");
        let yes = args.iter().position(|a| a == "--yes").expect("--yes");
        assert!(codecs < yes, "flags come before --yes");
        let default: Vec<String> = install_command("/dev/vda", "qq", &InstallOptions::default())
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!default.iter().any(|a| a == "--extension"));
    }

    #[test]
    fn stream_command_feeds_secrets_on_stdin_and_bounds_output() {
        {
            // Stub backend: echoes the password it read.
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg("read pw; echo \"pw=$pw\"; echo second");
            let mut lines = Vec::new();
            let ok = stream_command(cmd, "s3cret", &[], |l| lines.push(l)).unwrap();
            assert!(ok, "exit 0 should map to success");
            assert_eq!(lines, vec!["pw=s3cret".to_string(), "second".to_string()]);
        }
        {
            // The secret is read after the password, one line each.
            let mut cmd = Command::new("sh");
            cmd.arg("-c")
                .arg("read pw; read pin; echo \"pw=$pw\"; echo \"pin=$pin\"");
            let mut lines = Vec::new();
            let ok = stream_command(cmd, "s3cret", &["4242"], |l| lines.push(l)).unwrap();
            assert!(ok);
            assert_eq!(lines, vec!["pw=s3cret".to_string(), "pin=4242".to_string()]);
        }
        {
            // End to end: with encryption and a disk secret, the secret arrives as the second
            // stdin line.
            let opts = InstallOptions {
                pin: "9182".to_string().into(),
                ..InstallOptions::default()
            };
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
        {
            // No disk secret, so nothing extra goes on stdin.
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
        {
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

    #[test]
    fn unattended_credentials_are_transported_without_argv_or_policy_downgrade() {
        {
            let opts = InstallOptions {
                password_is_hash: true,
                force_startup_passphrase: true,
                ..InstallOptions::default()
            };
            let command = install_command("/dev/vdb", "ada", &opts);
            let args: Vec<&str> = command
                .get_args()
                .map(|arg| arg.to_str().unwrap())
                .collect();
            assert!(args.contains(&"--password-hash"));
            assert!(args.contains(&"--startup-passphrase-only"));
            assert!(
                !args.iter().any(|arg| arg.starts_with("$6$")),
                "a credential hash must not travel on argv"
            );
        }
        {
            let opts = InstallOptions {
                password_is_hash: true,
                prepared_setup: Some(install_profile_model::PreparedSetupExpectation {
                    schema: 1,
                    sealed_bytes: 48,
                    ciphertext_sha256: "0".repeat(64),
                }),
                ..InstallOptions::default()
            };
            let error = run_install("/dev/vdb", "ada", "$6$salt$AAAA", &opts, |_| {}).unwrap_err();
            assert!(error.to_string().contains("password hash"), "{error}");
        }
    }
}
