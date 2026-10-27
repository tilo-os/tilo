//! Explicit unattended installation from a private, relayed recipe.
//!
//! This is not a second automation surface. The recipe is read from an
//! inherited descriptor or a no-follow private file, validated field by field,
//! and then applied through the installer's published actions — the same
//! resolution, availability, confirmation and strict-binding door that a
//! control, a screen reader, `tilo-control actions invoke` and the console use.
//! There is no unattended-only operation that can select a disk, write a
//! credential or start an install behind that door.
//!
//! # What the recipe must carry
//!
//! * the destination's exact discovered identity (device, size, kernel device
//!   number, partition table and UUID, sector size, and WWN/serial when the
//!   hardware exposes them);
//! * either a plaintext account password or its canonical SHA-512 crypt hash,
//!   but never both, and never a target UID;
//! * a disk-unlock secret, with an explicit choice between the measured-boot
//!   TPM2 PIN policy and an independent strong passphrase;
//! * an explicit `data_source`, currently only `none` because no data transfer
//!   runs from this path;
//! * an explicit acknowledgement that the one-time recovery key is the
//!   operator's responsibility.
//!
//! Hardware-backed unlock is not inferred from a model name or a device node.
//! The canonical `tilo-rollback tpm-status` probe must answer, the installed
//! target preflight must accept the measured-boot boundary, and the real
//! `tilo-rollback enroll` helper repeats the proof after the pending disk has
//! actually been allocated. Sibling unlock is deliberately not enrolled here:
//! it needs an installed, signed-in session to certify and distribute the other
//! half, so this path does not fabricate a token it cannot validate.
//! The driver revalidates the exact disk against a fresh probe immediately
//! before `begin`; the privileged helper independently repeats every binding
//! before it writes anything. A changed fact refuses the operation and leaves
//! the ordinary wizard running behind the console socket, so a person can
//! continue interactively with the precise reason on screen.
//!
//! # Secrets
//!
//! Passwords and the disk secret never appear on argv, in the JSON state, in
//! action replies or in logs. The recipe bytes are zeroized after parsing, the
//! parsed secret strings are zeroized on drop, and the recovery key is written
//! only to an explicitly supplied private descriptor/file or to a real
//! terminal. Without such a destination the install does not start.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Map, Value};
use zeroize::{Zeroize, Zeroizing};

use crate::disks::Disk;
use crate::installer_app::{InstallerApp, validate_startup_secret_for};
use crate::{InstallState, Step};

/// The one recipe schema this build understands.
pub const SCHEMA: u32 = 1;

/// A recipe is a small control document, not a payload.
pub const MAX_RECIPE_BYTES: u64 = 64 * 1024;

const ROLLBACK_BIN: &str = "/usr/libexec/tilo/tilo-rollback";
const VERIFIED_BOOT_PREFLIGHT: &str = "/usr/libexec/tilo-verified-boot-install-preflight";
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// Where the recipe comes from. Neither source travels in argv content.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RecipeSource {
    File(PathBuf),
    Fd(RawFd),
}

/// Where the one-time recovery key is delivered. A missing destination is a
/// refusal before the first disk write, not a key hidden in a log.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RecoveryDestination {
    File(PathBuf),
    Fd(RawFd),
    Tty,
}

/// A parsed unattended invocation. The CLI only names descriptors and paths;
/// no credential is ever accepted as a command-line value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invocation {
    recipe: RecipeSource,
    recovery: RecoveryDestination,
}

impl Invocation {
    /// Parse the unattended subset of this binary's arguments. `Ok(None)` and
    /// no arguments starting with `--unattended` keeps the ordinary graphical
    /// installer path untouched.
    ///
    /// # Errors
    ///
    /// Duplicate sources, a missing value, an unparsable descriptor number, or
    /// a descriptor that would collide with stdin/stdout/stderr.
    pub fn from_args<I>(args: I) -> Result<Option<Self>, String>
    where
        I: IntoIterator<Item = OsString>,
    {
        let args: Vec<OsString> = args.into_iter().collect();
        if !args
            .iter()
            .any(|arg| arg.as_bytes().starts_with(b"--unattended"))
        {
            return Ok(None);
        }
        let mut recipe = None;
        let mut recovery_file = None;
        let mut recovery_fd = None;
        let mut index = 0;
        while let Some(arg) = args.get(index) {
            let flag = arg.to_string_lossy();
            match flag.as_ref() {
                "--unattended-file" => {
                    if recipe.is_some() {
                        return Err("give exactly one unattended recipe source".into());
                    }
                    recipe = Some(RecipeSource::File(private_path(
                        &next_value(&args, &mut index, &flag)?,
                        "recipe",
                    )?));
                }
                "--unattended-fd" => {
                    if recipe.is_some() {
                        return Err("give exactly one unattended recipe source".into());
                    }
                    recipe = Some(RecipeSource::Fd(private_fd(
                        &next_value(&args, &mut index, &flag)?,
                        "recipe",
                    )?));
                }
                "--unattended-recovery-file" => {
                    if recovery_file.is_some() || recovery_fd.is_some() {
                        return Err("give exactly one recovery-key destination".into());
                    }
                    recovery_file = Some(private_path(
                        &next_value(&args, &mut index, &flag)?,
                        "recovery key",
                    )?);
                }
                "--unattended-recovery-fd" => {
                    if recovery_file.is_some() || recovery_fd.is_some() {
                        return Err("give exactly one recovery-key destination".into());
                    }
                    recovery_fd = Some(private_fd(
                        &next_value(&args, &mut index, &flag)?,
                        "recovery key",
                    )?);
                }
                "--help" | "-h" => {
                    return Err(
                        "usage: tilo-installer --unattended-file PATH|--unattended-fd N \
                         [--unattended-recovery-file PATH|--unattended-recovery-fd N]"
                            .into(),
                    );
                }
                other => {
                    return Err(format!(
                        "unknown unattended argument {other:?}; give the recipe through \
                         --unattended-file or --unattended-fd, never as a command-line value"
                    ));
                }
            }
            index = index.saturating_add(1);
        }
        let recipe = recipe.ok_or("no unattended recipe source was given")?;
        let recovery = match (recovery_fd, recovery_file) {
            (Some(fd), None) => RecoveryDestination::Fd(fd),
            (None, Some(path)) => RecoveryDestination::File(path),
            (None, None) => RecoveryDestination::Tty,
            (Some(_), Some(_)) => {
                return Err("give exactly one recovery-key destination".into());
            }
        };
        Ok(Some(Self { recipe, recovery }))
    }

    /// Run this invocation to completion. On an error before the first disk
    /// write, the ordinary wizard stays alive behind the console socket; the
    /// process does not exit and does not hide a prompt.
    ///
    /// # Errors
    ///
    /// A command-line or console setup failure that leaves no interactive door.
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        let door = start_console(
            crate::operation::PROVIDER,
            console_control::Endpoint::discover(crate::operation::PROVIDER),
            stderr_is_terminal(),
            None,
        )
        .map_err(io::Error::other)?;

        let mut app = InstallerApp::for_unattended().with_console(door.console.clone());
        app.enable_unattended_mode();

        let recipe = match self.recipe.read() {
            Ok(recipe) => recipe,
            Err(error) => return stay_with_error(&mut app, &door, &error),
        };
        let mut sink = match self.recovery.open() {
            Ok(sink) => sink,
            Err(error) => return stay_with_error(&mut app, &door, &error),
        };
        let mut probe = SystemProtection;
        match execute(&mut app, &recipe, &mut probe, &mut sink) {
            Ok(()) => {
                eprintln!(
                    "Unattended installation finished. The recovery key was delivered to its \
                     private destination."
                );
                Ok(())
            }
            Err(error) => {
                let reason = recipe.redact(&error);
                stay_with_error(&mut app, &door, &reason)
            }
        }
    }
}

fn next_value(args: &[OsString], index: &mut usize, flag: &str) -> Result<OsString, String> {
    *index = index.saturating_add(1);
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))
}

fn private_path(value: &OsString, what: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(value.as_os_str());
    if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
        return Err(format!("the {what} path must be absolute"));
    }
    Ok(path)
}

fn private_fd(value: &OsString, what: &str) -> Result<RawFd, String> {
    let text = value
        .to_str()
        .ok_or_else(|| format!("the {what} descriptor is not text"))?;
    let fd: RawFd = text
        .parse()
        .map_err(|_| format!("the {what} descriptor is not a number"))?;
    if fd < 3 {
        return Err(format!(
            "the {what} descriptor must be 3 or higher; standard streams are not recipe channels"
        ));
    }
    Ok(fd)
}

/// The console endpoints this process actually bound. A request wakes the
/// owner through the channel, so the fallback listener never depends on a
/// polling sleep to notice one.
struct ConsoleDoor {
    console: console_control::ConsoleServer,
    endpoints: Vec<console_control::Endpoint>,
    wake: mpsc::Receiver<()>,
    /// Keeps a fallback socket's private directory alive for as long as the
    /// door serves it. The directory is removed when the process exits.
    _fallback: Option<tempfile::TempDir>,
}

impl ConsoleDoor {
    #[cfg(test)]
    fn socket_path(&self) -> Option<&Path> {
        self.endpoints.iter().find_map(|endpoint| match endpoint {
            console_control::Endpoint::Socket(path) => Some(path.as_path()),
            console_control::Endpoint::Port(_) => None,
        })
    }

    fn instructions(&self) -> Vec<String> {
        self.endpoints
            .iter()
            .map(|endpoint| match endpoint {
                console_control::Endpoint::Socket(path) => {
                    format!("tilo-control --socket {} actions list", path.display())
                }
                console_control::Endpoint::Port(path) => {
                    format!("the console channel at {}", path.display())
                }
            })
            .collect()
    }
}

fn endpoint_path(endpoint: &console_control::Endpoint) -> &Path {
    match endpoint {
        console_control::Endpoint::Socket(path) | console_control::Endpoint::Port(path) => path,
    }
}

/// Bind the discovered endpoints, or a private fallback socket when this is a
/// real terminal session and no discovered endpoint could be bound. The
/// fallback uses the same `ConsoleServer`/`Endpoint` door and the same actions
/// as every other console caller; it is not a second automation surface.
fn start_console(
    app: &str,
    discovered: Vec<console_control::Endpoint>,
    terminal_available: bool,
    fallback_root: Option<PathBuf>,
) -> Result<ConsoleDoor, String> {
    let console = console_control::ConsoleServer::new();
    let (wake_tx, wake) = mpsc::channel();
    let waker: console_control::server::Waker = Arc::new(move || {
        let _ = wake_tx.send(());
    });
    let mut endpoints = Vec::new();
    let mut failures = Vec::new();
    for endpoint in discovered {
        match console.serve(&endpoint, Arc::clone(&waker)) {
            Ok(()) => endpoints.push(endpoint),
            Err(error) => failures.push(format!("{}: {error}", endpoint_path(&endpoint).display())),
        }
    }
    let mut fallback = None;
    if endpoints.is_empty() {
        if !terminal_available {
            let attempts = if failures.is_empty() {
                "no console socket was discovered".to_owned()
            } else {
                format!(
                    "the console endpoint(s) could not be opened: {}",
                    failures.join("; ")
                )
            };
            return Err(format!(
                "No console socket is available, so an unattended failure could not be \
                 continued interactively ({attempts}). Run this in the installer's session, \
                 attach its console channel, or run it from a terminal so a private fallback \
                 socket can be opened."
            ));
        }
        let root = fallback_root.unwrap_or_else(|| {
            std::env::var_os("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute() && path.is_dir())
                .unwrap_or_else(std::env::temp_dir)
        });
        let directory = tempfile::Builder::new()
            .prefix("tilo-console-")
            .tempdir_in(&root)
            .map_err(|error| {
                format!(
                    "No console endpoint could be bound and a private fallback directory in {} \
                     could not be created: {error}",
                    root.display()
                )
            })?;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|error| {
                format!("A private fallback console directory could not be secured: {error}")
            })?;
        let endpoint =
            console_control::Endpoint::Socket(directory.path().join(format!("{app}.sock")));
        console
            .serve(&endpoint, Arc::clone(&waker))
            .map_err(|error| {
                format!(
                    "No console endpoint could be bound and the fallback socket {} could not be \
                     opened: {error}",
                    endpoint_path(&endpoint).display()
                )
            })?;
        endpoints.push(endpoint);
        fallback = Some(directory);
    }
    Ok(ConsoleDoor {
        console,
        endpoints,
        wake,
        _fallback: fallback,
    })
}

/// Answer every request queued so far, returning how many were answered.
fn drain_console(app: &mut InstallerApp, console: &console_control::ConsoleServer) -> usize {
    let pending = console.take_pending();
    let answered = pending.len();
    for pending in pending {
        let reply = app.answer_console(pending.line());
        pending.respond(reply);
    }
    answered
}

/// Keep the ordinary installer available after an unattended refusal.
fn stay_with_error(
    app: &mut InstallerApp,
    door: &ConsoleDoor,
    error: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    app.note_unattended_error(error.to_owned());
    eprintln!("Unattended installation stopped: {error}");
    eprintln!("The installer is still available through its console:");
    for instruction in door.instructions() {
        eprintln!("  {instruction}");
    }
    eprintln!("Invoke the action for the step this report names.");
    serve_forever(app, door);
}

/// Answer console requests on this thread. Unattended runs have no winit event
/// loop, so this is the same `answer_console` entry point woken by the console
/// listener rather than by a winit proxy.
fn serve_forever(app: &mut InstallerApp, door: &ConsoleDoor) -> ! {
    loop {
        drain_console(app, &door.console);
        let _ = door.wake.recv_timeout(Duration::from_secs(1));
    }
}

fn stderr_is_terminal() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
}

// ===================================================================
// Recipe
// ===================================================================

/// A fully validated unattended recipe. Not `Debug`: it holds credentials.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    schema: u32,
    account: AccountRecipe,
    disk: DiskRecipe,
    protection: ProtectionRecipe,
    /// No data-transfer implementation runs from this path yet. Requiring the
    /// field makes "none" a reviewed choice rather than a silent omission.
    data_source: String,
    /// The operator will take custody of the one-time recovery key.
    acknowledge_recovery_key: bool,
    #[serde(default)]
    preferences: Preferences,
    finish: Finish,
}

impl Drop for Recipe {
    fn drop(&mut self) {
        self.account.zeroize();
        self.protection.zeroize();
    }
}

impl Recipe {
    fn parse(bytes: &[u8]) -> Result<Self, String> {
        let recipe: Self = serde_json::from_slice(bytes).map_err(|_| {
            "The unattended recipe is malformed or has unsupported fields".to_string()
        })?;
        recipe.validate()?;
        Ok(recipe)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "The unattended recipe uses schema {}; this installer understands {SCHEMA}",
                self.schema
            ));
        }
        self.account.validate()?;
        self.disk.validate()?;
        self.protection.validate()?;
        if self.data_source != "none" {
            return Err(format!(
                "Unattended data source {:?} is not implemented. Leave the data source out or set \
                 it to \"none\"; choose data interactively if it must be transferred.",
                self.data_source
            ));
        }
        if !self.acknowledge_recovery_key {
            return Err(
                "The unattended recipe must acknowledge that its operator will take custody of \
                 the one-time recovery key"
                    .into(),
            );
        }
        self.preferences.validate()
    }

    fn redact(&self, message: &str) -> String {
        let mut output = message.to_owned();
        for secret in self.secrets() {
            if !secret.is_empty() {
                output = output.replace(secret, "[redacted]");
            }
        }
        output
    }

    fn secrets(&self) -> Vec<&str> {
        let mut secrets = vec![self.protection.secret.as_str()];
        if let Some(password) = self.account.password.as_deref() {
            secrets.push(password);
        }
        if let Some(hash) = self.account.password_sha512_crypt.as_deref() {
            secrets.push(hash);
        }
        secrets
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountRecipe {
    username: String,
    #[serde(default)]
    full_name: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    password_sha512_crypt: Option<String>,
}

impl AccountRecipe {
    fn validate(&self) -> Result<(), String> {
        if !install_profile_model::valid_username(&self.username) {
            return Err(
                "The unattended account name must be a portable, non-reserved account name".into(),
            );
        }
        if !install_profile_model::valid_full_name(&self.full_name) {
            return Err("The unattended account's full name is not valid".into());
        }
        match (&self.password, &self.password_sha512_crypt) {
            (Some(password), None) => {
                if password.is_empty()
                    || password.len() > 4096
                    || password
                        .bytes()
                        .any(|byte| matches!(byte, b'\n' | b'\r' | 0))
                {
                    return Err(
                        "The unattended account password is empty or contains a line break".into(),
                    );
                }
            }
            (None, Some(hash)) if valid_sha512_crypt(hash) => {}
            (None, Some(_)) => {
                return Err(
                    "The unattended account credential is not a canonical SHA-512 crypt hash"
                        .into(),
                );
            }
            _ => {
                return Err(
                    "The unattended account needs exactly one credential: `password` or \
                     `password_sha512_crypt`"
                        .into(),
                );
            }
        }
        Ok(())
    }

    fn zeroize(&mut self) {
        if let Some(password) = self.password.as_mut() {
            password.zeroize();
        }
        if let Some(hash) = self.password_sha512_crypt.as_mut() {
            hash.zeroize();
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DiskRecipe {
    device: String,
    /// The explicit plan. Only a reviewed whole-disk erase is implemented
    /// unattended; an alongside allocation still needs the interactive
    /// preflight and is refused rather than guessed.
    method: String,
    size_bytes: u64,
    major_minor: String,
    #[serde(default)]
    wwn: String,
    #[serde(default)]
    serial: String,
    partition_table: String,
    partition_table_uuid: String,
    logical_sector_size: u64,
}

impl DiskRecipe {
    fn validate(&self) -> Result<(), String> {
        if self.method != "erase" {
            return Err(format!(
                "Unattended installation supports only the explicit `erase` disk plan, not {:?}; \
                 continue interactively to review an alongside allocation",
                self.method
            ));
        }
        if !self.device.starts_with("/dev/")
            || self.device.len() > 256
            || self
                .device
                .bytes()
                .any(|byte| byte.is_ascii_control() || byte == b' ')
        {
            return Err("The unattended destination is not a concrete /dev device".into());
        }
        if self.size_bytes == 0 || self.logical_sector_size == 0 {
            return Err(
                "The unattended disk entry needs its exact size and logical sector size".into(),
            );
        }
        if !valid_major_minor(&self.major_minor) {
            return Err(
                "The unattended disk entry needs the reviewed MAJ:MIN kernel device number".into(),
            );
        }
        if !matches!(self.partition_table.as_str(), "" | "gpt") {
            return Err(format!(
                "Unsupported reviewed partition table {:?}",
                self.partition_table
            ));
        }
        if self.partition_table == "gpt"
            && !crate::internal_handoff::is_guid(&self.partition_table_uuid)
        {
            return Err("A reviewed GPT destination needs its exact partition-table UUID".into());
        }
        if self.partition_table != "gpt" && !self.partition_table_uuid.is_empty() {
            return Err("A non-GPT destination cannot carry a GPT partition-table UUID".into());
        }
        Ok(())
    }
}

fn valid_major_minor(value: &str) -> bool {
    let Some((major, minor)) = value.split_once(':') else {
        return false;
    };
    !major.is_empty()
        && !minor.is_empty()
        && major.bytes().all(|byte| byte.is_ascii_digit())
        && minor.bytes().all(|byte| byte.is_ascii_digit())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProtectionRecipe {
    /// `tpm2` requires the actual measured-boot TPM2 boundary; `passphrase`
    /// forces an independent strong LUKS passphrase even where the TPM is
    /// usable, never weakening the secret.
    kind: String,
    secret: String,
}

impl ProtectionRecipe {
    fn validate(&self) -> Result<(), String> {
        let tpm_pin = match self.kind.as_str() {
            "tpm2" => true,
            "passphrase" => false,
            other => {
                return Err(format!(
                    "Unattended protection must be `tpm2` or `passphrase`, not {other:?}"
                ));
            }
        };
        validate_startup_secret_for(&self.secret, tpm_pin).map_err(str::to_owned)
    }

    fn zeroize(&mut self) {
        self.secret.zeroize();
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preferences {
    #[serde(default = "default_hostname")]
    hostname: String,
    #[serde(default = "default_locale")]
    locale: String,
    #[serde(default = "default_timezone")]
    timezone: String,
    #[serde(default = "default_keyboard")]
    keyboard: String,
    #[serde(default = "default_filesystem")]
    filesystem: String,
    #[serde(default)]
    codecs: bool,
    #[serde(default)]
    fido2: bool,
}

impl Preferences {
    fn validate(&self) -> Result<(), String> {
        if !install_profile_model::valid_hostname(&self.hostname) {
            return Err("The unattended hostname is not valid".into());
        }
        if !install_profile_model::valid_locale(&self.locale) {
            return Err("The unattended locale is not in the installer's catalog".into());
        }
        if !install_profile_model::valid_timezone_name(&self.timezone) {
            return Err("The unattended time zone is not a relative IANA name".into());
        }
        if !install_profile_model::KEYBOARDS.contains(&self.keyboard.as_str()) {
            return Err("The unattended keyboard layout is not offered by the installer".into());
        }
        if !install_profile_model::FILESYSTEMS.contains(&self.filesystem.as_str()) {
            return Err("The unattended filesystem is not offered by the installer".into());
        }
        Ok(())
    }

    fn keyboard_display_name(&self) -> Result<&'static str, String> {
        crate::layouts::LAYOUTS
            .iter()
            .find(|layout| layout.id == self.keyboard)
            .map(|layout| layout.display_name)
            .ok_or_else(|| "The unattended keyboard layout is not offered by the installer".into())
    }
}

fn default_hostname() -> String {
    "tilo".into()
}
fn default_locale() -> String {
    "en_US.UTF-8".into()
}
fn default_timezone() -> String {
    "UTC".into()
}
fn default_keyboard() -> String {
    "us".into()
}
fn default_filesystem() -> String {
    "btrfs".into()
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Finish {
    Stay,
    Reboot,
    PowerOff,
}

impl Finish {
    fn power_action(self) -> Option<&'static str> {
        match self {
            Self::Stay => None,
            Self::Reboot => Some("reboot@installer.power"),
            Self::PowerOff => Some("power-off@installer.power"),
        }
    }
}

/// A canonical SHA-512 crypt hash: the same form `tilo-firstboot-user` installs
/// through `chpasswd --encrypted` and the privileged installer already records.
pub(crate) fn valid_sha512_crypt(hash: &str) -> bool {
    fn crypt_b64(byte: u8) -> bool {
        matches!(byte, b'.' | b'/' | b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z')
    }
    let Some(rest) = hash.strip_prefix("$6$") else {
        return false;
    };
    let rest = if let Some(rounds) = rest.strip_prefix("rounds=") {
        let Some((value, rest)) = rounds.split_once('$') else {
            return false;
        };
        if value.is_empty()
            || value.len() > 9
            || value.starts_with('0')
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        let Ok(rounds) = value.parse::<u32>() else {
            return false;
        };
        if !(1000..=999_999_999).contains(&rounds) {
            return false;
        }
        rest
    } else {
        rest
    };
    let Some((salt, digest)) = rest.split_once('$') else {
        return false;
    };
    !salt.is_empty()
        && salt.len() <= 16
        && salt.bytes().all(crypt_b64)
        && digest.len() == 86
        && digest.bytes().all(crypt_b64)
}

impl RecipeSource {
    fn read(&self) -> Result<Recipe, String> {
        let bytes = match self {
            Self::File(path) => read_private_file(path)?,
            Self::Fd(fd) => read_private_fd(*fd)?,
        };
        if bytes.len() as u64 > MAX_RECIPE_BYTES {
            return Err("The unattended recipe is larger than the accepted bound".into());
        }
        Recipe::parse(bytes.as_slice())
    }
}

fn read_private_fd(fd: RawFd) -> Result<Zeroizing<Vec<u8>>, String> {
    // SAFETY: the caller named an inherited descriptor; dup gives this process
    // an independently owned one without closing theirs.
    let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicated < 0 {
        return Err(format!(
            "The unattended recipe descriptor {fd} is not readable: {}",
            io::Error::last_os_error()
        ));
    }
    // SAFETY: fcntl returned a fresh descriptor, now owned by File.
    let mut file = unsafe { std::fs::File::from_raw_fd(duplicated) };
    let metadata = file.metadata().map_err(|error| {
        format!("The unattended recipe descriptor {fd} is not readable: {error}")
    })?;
    if metadata.is_dir() {
        return Err("The unattended recipe descriptor names a directory".into());
    }
    read_bounded(&mut file, "recipe")
}

fn read_private_file(path: &Path) -> Result<Zeroizing<Vec<u8>>, String> {
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "The unattended recipe path contains a NUL".to_string())?;
    // SAFETY: c_path is NUL terminated and open returns a new descriptor.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "The unattended recipe at {} could not be opened without following a link: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    // SAFETY: open returned a fresh descriptor, now owned by File.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("The unattended recipe could not be inspected: {error}"))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(
            "The unattended recipe must be a single regular file, not a link or device".into(),
        );
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(
            "The unattended recipe is readable by another account; make it mode 0600".into(),
        );
    }
    let uid = metadata.uid();
    let euid = rustix::process::geteuid().as_raw();
    if uid != euid && uid != 0 {
        return Err("The unattended recipe is not owned by this installer or root".into());
    }
    read_bounded(&mut file, "recipe")
}

fn read_bounded(file: &mut std::fs::File, what: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_RECIPE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("The {what} could not be read: {error}"))?;
    if bytes.len() as u64 > MAX_RECIPE_BYTES {
        return Err(format!("The {what} is larger than the accepted bound"));
    }
    Ok(bytes)
}

// ===================================================================
// Destination protection
// ===================================================================

/// What the canonical probes established about the actual destination.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each field is an independent probe result, not a state combination"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProtectionReport {
    pub tpm2: bool,
    pub pin_defensible: bool,
    pub lockout_auth_set: bool,
    pub shared_with_windows: bool,
    pub target_verified_boot: bool,
}

trait ProtectionProbe {
    fn probe(&mut self) -> Result<ProtectionReport, String>;
}

struct SystemProtection;

impl ProtectionProbe for SystemProtection {
    fn probe(&mut self) -> Result<ProtectionReport, String> {
        // A real TPM2 protocol read, not a device-node presence test. The
        // dictionary-attack capability is what makes a short PIN defensible;
        // the first boot hardens it, and the value is reported to the operator.
        let rollback = bounded_command::run(
            ROLLBACK_BIN,
            &["tpm-status", "--json"],
            None,
            64 * 1024,
            PROBE_TIMEOUT,
        )
        .map_err(|error| format!("The destination TPM2 could not be checked: {error}"))?;
        if !rollback.status.success() {
            return Err(format!(
                "The destination does not answer the canonical TPM2 probe (this path needs TPM2                  access, normally by running the unattended entry point as root): {}",
                rollback.stderr_diagnostic()
            ));
        }
        let status: Value = serde_json::from_slice(&rollback.stdout)
            .map_err(|_| "The destination TPM2 probe returned an unreadable answer".to_string())?;
        let pin_defensible = status
            .get("pin_is_defensible")
            .and_then(Value::as_bool)
            .ok_or("The destination TPM2 probe did not report its lockout policy")?;
        let lockout_auth_set = status
            .get("reset_auth_set")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let shared_with_windows = status
            .get("shared_with_windows")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let preflight = bounded_command::run(
            VERIFIED_BOOT_PREFLIGHT,
            &[],
            None,
            64 * 1024,
            PROBE_TIMEOUT,
        )
        .map_err(|error| {
            format!("The installed target's measured-boot boundary could not be checked: {error}")
        })?;
        if !preflight.status.success() {
            return Err(format!(
                "The installed target does not prove the measured-boot boundary this PIN would \
                 be sealed to: {}",
                preflight.stderr_diagnostic()
            ));
        }
        Ok(ProtectionReport {
            tpm2: true,
            pin_defensible,
            lockout_auth_set,
            shared_with_windows,
            target_verified_boot: true,
        })
    }
}

// ===================================================================
// Execution through published actions
// ===================================================================

fn execute(
    app: &mut InstallerApp,
    recipe: &Recipe,
    probe: &mut dyn ProtectionProbe,
    sink: &mut RecoverySink,
) -> Result<(), String> {
    if recipe.protection.kind == "tpm2" {
        let report = probe.probe()?;
        if !report.tpm2 || !report.target_verified_boot {
            return Err("The destination did not prove a usable hardware-backed unlock".into());
        }
        if !app.tpm_pin_policy() {
            return Err(
                "The installer's own target preflight did not accept the measured-boot boundary; \
                 install the same reviewed target and retry, or choose the passphrase form"
                    .into(),
            );
        }
        if !report.pin_defensible {
            eprintln!(
                "Unattended installation: the destination TPM2 dictionary-attack reset \
                 authorisation was not set at review time. {} Without that, a short PIN is not \
                 a defensible unlock.",
                if report.shared_with_windows {
                    "This TPM is shared with Windows, which sets it the next time it starts."
                } else {
                    "The installed system hardens it on first boot."
                }
            );
        }
    }

    drive(app, recipe)?;
    if app.install_state() != InstallState::Done {
        let tail = app
            .console_state()
            .pointer("/install/log_tail")
            .and_then(Value::as_array)
            .and_then(|lines| lines.last())
            .and_then(Value::as_str)
            .unwrap_or("the privileged helper did not report success")
            .to_owned();
        return Err(format!("The installation failed: {tail}"));
    }

    let key = app
        .recovery_key()
        .ok_or("The installed system reported success without a recovery key")?
        .to_owned();
    sink.write(&key)?;
    drop(Zeroizing::new(key));
    invoke(
        app,
        "confirm-saved@installer.recovery",
        serde_json::json!({"saved": true}),
    )?;
    if let Some(action) = recipe.finish.power_action() {
        invoke(app, action, serde_json::json!({}))?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per wizard step keeps the published-action sequence together"
)]
fn drive(app: &mut InstallerApp, recipe: &Recipe) -> Result<(), String> {
    for _ in 0..32 {
        let step = app.step();
        match step {
            Step::Keyboard => {
                invoke(
                    app,
                    "set-layout@installer.keyboard",
                    serde_json::json!({"layout": recipe.preferences.keyboard_display_name()?}),
                )?;
                advance(app, step)?;
            }
            Step::Network => advance(app, step)?,
            Step::Accounts => {
                // This is the published offline choice; it advances on its own.
                invoke(
                    app,
                    "skip-offline@installer.accounts",
                    serde_json::json!({}),
                )?;
                if app.step() == step {
                    return Err(step_error(app, step));
                }
            }
            Step::Disk => {
                // Re-read the real inventory, then bind the recipe to it.
                invoke(app, "refresh@installer.storage", serde_json::json!({}))?;
                let disk = reviewed_disk(app.available_disks(), &recipe.disk)?.to_owned();
                invoke(
                    app,
                    "set-method@installer.storage",
                    serde_json::json!({"method": "erase"}),
                )?;
                invoke(
                    app,
                    &format!("select@{}", crate::operation::disk_resource(&disk)),
                    serde_json::json!({}),
                )?;
                advance(app, step)?;
            }
            Step::Account => {
                let mode = recipe.protection.kind.as_str();
                invoke(
                    app,
                    "set-disk-passphrase@installer.account",
                    serde_json::json!({
                        "passphrase": recipe.protection.secret,
                        "confirmation": recipe.protection.secret,
                        "mode": mode,
                    }),
                )?;
                if let Some(hash) = recipe.account.password_sha512_crypt.as_deref() {
                    invoke(
                        app,
                        "set-password-hash@installer.account",
                        serde_json::json!({"hash": hash}),
                    )?;
                } else if let Some(password) = recipe.account.password.as_deref() {
                    invoke(
                        app,
                        "set-passphrase@installer.account",
                        serde_json::json!({
                            "passphrase": password,
                            "confirmation": password,
                        }),
                    )?;
                }
                invoke(
                    app,
                    "set-identity@installer.account",
                    serde_json::json!({
                        "username": recipe.account.username,
                        "full_name": recipe.account.full_name,
                    }),
                )?;
                advance(app, step)?;
            }
            Step::Options => {
                let preferences = &recipe.preferences;
                invoke(
                    app,
                    "set-hostname@installer.options",
                    serde_json::json!({"hostname": preferences.hostname}),
                )?;
                invoke(
                    app,
                    "set-locale@installer.options",
                    serde_json::json!({"locale": preferences.locale}),
                )?;
                invoke(
                    app,
                    "set-timezone@installer.options",
                    serde_json::json!({"timezone": preferences.timezone}),
                )?;
                invoke(
                    app,
                    "set-filesystem@installer.options",
                    serde_json::json!({"filesystem": preferences.filesystem}),
                )?;
                invoke(
                    app,
                    "set-security-key@installer.options",
                    serde_json::json!({"enabled": preferences.fido2}),
                )?;
                advance(app, step)?;
            }
            Step::Media => {
                if recipe.preferences.codecs {
                    invoke(
                        app,
                        "set-media-codecs@installer.media",
                        serde_json::json!({"enabled": true}),
                    )?;
                }
                advance(app, step)?;
            }
            Step::Review => {
                // The final authority is a fresh probe of the actual block
                // device immediately before the destructive action.
                reviewed_disk(&app.probe_disks(), &recipe.disk)?;
                invoke(
                    app,
                    "set-recovery-promise@installer.review",
                    serde_json::json!({"acknowledged": true}),
                )?;
                let target = app
                    .console_state()
                    .pointer("/storage/confirmation_target")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| "The installer names no reviewed target".to_string())?;
                invoke(
                    app,
                    "begin@installer.review",
                    serde_json::json!({"confirmation_phrase": target}),
                )?;
                return Ok(());
            }
            Step::Install => {
                return Err("The installer reached Install before Review could be approved".into());
            }
        }
    }
    Err("The installer did not reach Review after 32 reviewed actions".into())
}

fn advance(app: &mut InstallerApp, step: Step) -> Result<(), String> {
    invoke(app, "next@installer", serde_json::json!({}))?;
    if app.step() == step {
        return Err(step_error(app, step));
    }
    Ok(())
}

fn step_error(app: &InstallerApp, step: Step) -> String {
    let state = app.console_state();
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .filter(|status| !status.trim().is_empty())
        .unwrap_or("the installer did not advance");
    format!("The {} step could not continue: {status}", step_label(step))
}

fn step_label(step: Step) -> &'static str {
    match step {
        Step::Keyboard => "keyboard",
        Step::Network => "network",
        Step::Accounts => "accounts",
        Step::Disk => "disk",
        Step::Account => "account",
        Step::Options => "options",
        Step::Media => "media",
        Step::Review => "review",
        Step::Install => "install",
    }
}

fn invoke(app: &mut InstallerApp, id: &str, args: Value) -> Result<(), String> {
    let args: Map<String, Value> = match args {
        Value::Object(args) => args,
        _ => Map::new(),
    };
    app.invoke_published(id, &args).map(|_| ())
}

/// The one discovered disk the recipe names, with every reviewed identity
/// field still identical. Anything else is a refusal, never a substitution.
#[allow(
    clippy::suspicious_operation_groupings,
    reason = "each line compares one independent reviewed identity field"
)]
fn reviewed_disk<'a>(disks: &'a [Disk], recipe: &DiskRecipe) -> Result<&'a str, String> {
    let matching: Vec<&Disk> = disks
        .iter()
        .filter(|disk| {
            disk.name == recipe.device
                && disk.size_bytes == recipe.size_bytes
                && disk.major_minor == recipe.major_minor
                && disk.wwn == recipe.wwn
                && disk.serial == recipe.serial
                && disk.partition_table == recipe.partition_table
                && disk.partition_table_uuid == recipe.partition_table_uuid
                && disk.logical_sector_size == recipe.logical_sector_size
        })
        .collect();
    match matching.as_slice() {
        [disk] if disk.blocks_install() || disk.read_only => Err(format!(
            "The reviewed destination {} is the running installer medium or is read-only",
            recipe.device
        )),
        [disk] => Ok(disk.name.as_str()),
        [] => Err(format!(
            "The reviewed destination {} no longer matches the actual block device: expected \
             {} bytes, MAJ:MIN {}, {:?} table{}, but that exact identity is not attached. No disk \
             was changed.",
            recipe.device,
            recipe.size_bytes,
            recipe.major_minor,
            recipe.partition_table,
            if recipe.wwn.is_empty() && recipe.serial.is_empty() {
                String::new()
            } else {
                format!(" and identifiers {}/{}", recipe.wwn, recipe.serial)
            }
        )),
        _ => Err(format!(
            "More than one attached disk matches the reviewed destination {}; attach only the \
             reviewed disk and retry",
            recipe.device
        )),
    }
}

// ===================================================================
// Recovery-key delivery
// ===================================================================

struct RecoverySink {
    inner: SinkInner,
}

enum SinkInner {
    File(std::fs::File),
    Tty,
}

impl RecoveryDestination {
    fn open(self) -> Result<RecoverySink, String> {
        let inner = match self {
            Self::File(path) => SinkInner::File(open_private_output(&path)?),
            Self::Fd(fd) => {
                let duplicated = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
                if duplicated < 0 {
                    return Err(format!(
                        "The recovery-key descriptor {fd} is not writable: {}",
                        io::Error::last_os_error()
                    ));
                }
                // SAFETY: fcntl returned a fresh descriptor, now owned by File.
                SinkInner::File(unsafe { std::fs::File::from_raw_fd(duplicated) })
            }
            Self::Tty => {
                if !stderr_is_terminal() {
                    return Err(
                        "No private recovery-key destination was supplied and standard error is \
                         not a terminal. Pass --unattended-recovery-fd or \
                         --unattended-recovery-file; the installer will not start an install \
                         whose only unlock fallback would be lost."
                            .into(),
                    );
                }
                SinkInner::Tty
            }
        };
        Ok(RecoverySink { inner })
    }
}

impl RecoverySink {
    fn write(&mut self, key: &str) -> Result<(), String> {
        match &mut self.inner {
            SinkInner::File(file) => writeln!(file, "{key}")
                .and_then(|()| file.flush())
                .and_then(|()| {
                    if file.metadata().is_ok_and(|metadata| metadata.is_file()) {
                        file.sync_all()
                    } else {
                        Ok(())
                    }
                })
                .map_err(|error| format!("The recovery key could not be written: {error}")),
            SinkInner::Tty => {
                eprintln!("Recovery key (store this off this computer): {key}");
                Ok(())
            }
        }
    }
}

fn open_private_output(path: &Path) -> Result<std::fs::File, String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent_info = std::fs::symlink_metadata(parent).map_err(|error| {
        format!(
            "The recovery-key directory {} is unavailable: {error}",
            parent.display()
        )
    })?;
    if !parent_info.is_dir() || parent_info.file_type().is_symlink() {
        return Err(format!(
            "The recovery-key directory {} is not a plain directory",
            parent.display()
        ));
    }
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| "The recovery-key path contains a NUL".to_string())?;
    // SAFETY: c_path is NUL terminated; O_EXCL and O_NOFOLLOW keep a
    // pre-existing path or a final symlink from redirecting the key.
    let fd = unsafe {
        libc::open(
            c_path.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if fd < 0 {
        return Err(format!(
            "The recovery-key file {} could not be created privately: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    // SAFETY: open returned a fresh descriptor, now owned by File.
    Ok(unsafe { std::fs::File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::too_many_lines,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;
    use crate::backend::InstallBackend;
    use crate::install::{InstallOptions, PowerAction};
    use crate::installer_app::Protection;
    use serde_json::json;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixStream;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "bbbbbbbb-cccccccc-dddddddd-ffffffff-gggggggg-hhhhhhhh-jjjjjjjj-kkkkkkkk";
    const DISK_SECRET: &str = "correct horse battery staple";
    const PIN: &str = "739184";

    fn account_hash() -> String {
        format!("$6$salt${}", "A".repeat(86))
    }

    fn disk() -> Disk {
        Disk {
            name: "/dev/vdb".into(),
            size: "40G".into(),
            model: "QEMU HARDDISK".into(),
            size_bytes: 40 * 1024 * 1024 * 1024,
            partition_table: String::new(),
            partition_table_uuid: String::new(),
            logical_sector_size: 512,
            major_minor: "252:16".into(),
            wwn: String::new(),
            serial: "tilo-install-target".into(),
            removable: false,
            read_only: false,
            is_system: false,
            partitions: Vec::new(),
            live_system_nodes: Vec::new(),
            internal_plan: None,
        }
    }

    type InstallLog = Arc<Mutex<Vec<(String, String, String, InstallOptions)>>>;

    struct Refuse;

    impl ProtectionProbe for Refuse {
        fn probe(&mut self) -> Result<ProtectionReport, String> {
            Err("the actual TPM2 did not answer".into())
        }
    }

    #[derive(Clone, Default)]
    struct Fake {
        installs: InstallLog,
        power: Arc<Mutex<Vec<PowerAction>>>,
        succeed: Arc<Mutex<bool>>,
    }

    impl Fake {
        fn succeeding() -> Self {
            let fake = Self::default();
            *fake.succeed.lock().unwrap() = true;
            fake
        }
    }

    impl InstallBackend for Fake {
        fn read_disks(&self) -> Vec<Disk> {
            vec![disk()]
        }

        fn run_install(
            &self,
            disk: &str,
            user: &str,
            password: &str,
            opts: &InstallOptions,
            on_line: &mut dyn FnMut(String),
        ) -> std::io::Result<bool> {
            self.installs.lock().unwrap().push((
                disk.into(),
                user.into(),
                password.into(),
                opts.clone(),
            ));
            on_line(format!("::tilo-recovery-key::{KEY}"));
            on_line("==> Preparing the account on the installed system".into());
            Ok(*self.succeed.lock().unwrap())
        }

        fn run_alongside_preflight(
            &self,
            _disk: &str,
            _opts: &InstallOptions,
            _on_line: &mut dyn FnMut(String),
        ) -> std::io::Result<bool> {
            Ok(true)
        }

        fn run_power(&self, action: PowerAction) -> std::io::Result<bool> {
            self.power.lock().unwrap().push(action);
            Ok(true)
        }
    }

    struct Probe(ProtectionReport);

    impl ProtectionProbe for Probe {
        fn probe(&mut self) -> Result<ProtectionReport, String> {
            Ok(self.0.clone())
        }
    }

    fn recipe(hash: bool) -> serde_json::Value {
        let mut account = json!({
            "username": "ada",
            "full_name": "Ada Lovelace",
        });
        if hash {
            account["password_sha512_crypt"] = json!(account_hash());
        } else {
            account["password"] = json!("ordinary typed password");
        }
        json!({
            "schema": SCHEMA,
            "account": account,
            "disk": {
                "device": "/dev/vdb",
                "method": "erase",
                "size_bytes": 40u64 * 1024 * 1024 * 1024,
                "major_minor": "252:16",
                "serial": "tilo-install-target",
                "partition_table": "",
                "partition_table_uuid": "",
                "logical_sector_size": 512,
            },
            "protection": {"kind": "passphrase", "secret": DISK_SECRET},
            "data_source": "none",
            "acknowledge_recovery_key": true,
            "preferences": {
                "hostname": "lab-1",
                "locale": "en_US.UTF-8",
                "timezone": "UTC",
                "keyboard": "us",
                "filesystem": "xfs",
            },
            "finish": "stay",
        })
    }

    fn build_app(fake: &Fake, protection: Protection) -> InstallerApp {
        let mut app = InstallerApp::with_disks(Vec::new())
            .with_backend(Arc::new(fake.clone()))
            .with_protection_for_test(protection);
        app.enable_unattended_mode();
        app
    }

    fn parse(value: &serde_json::Value) -> Recipe {
        Recipe::parse(serde_json::to_vec(value).unwrap().as_slice()).unwrap()
    }

    #[test]
    fn a_private_hash_recipe_installs_through_published_actions() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: false,
                secure_boot: false,
                kernel_lockdown: false,
                target_verified_boot: false,
            },
        );
        let recipe = parse(&recipe(true));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery-key");
        let mut sink = RecoveryDestination::File(path.clone()).open().unwrap();
        let mut probe = Probe(ProtectionReport {
            tpm2: false,
            pin_defensible: false,
            lockout_auth_set: false,
            shared_with_windows: false,
            target_verified_boot: false,
        });

        execute(&mut app, &recipe, &mut probe, &mut sink).unwrap();

        let installs = fake.installs.lock().unwrap().clone();
        assert_eq!(installs.len(), 1);
        let (target, user, password, options) = &installs[0];
        assert_eq!((target.as_str(), user.as_str()), ("/dev/vdb", "ada"));
        assert_eq!(password, &account_hash());
        assert!(options.password_is_hash);
        assert!(options.force_startup_passphrase);
        assert_eq!(options.filesystem, "xfs");
        assert_eq!(options.hostname, "lab-1");
        assert_eq!(options.pin.as_str(), DISK_SECRET);
        assert_eq!(app.install_state(), InstallState::Done);

        let delivered = std::fs::read_to_string(&path).unwrap();
        assert_eq!(delivered.trim(), KEY);
        let state = app.console_state().to_string();
        assert!(
            !state.contains(&account_hash()),
            "the hash reached state: {state}"
        );
        assert!(
            !state.contains(DISK_SECRET),
            "the disk secret reached state"
        );
        assert!(!state.contains(KEY), "the recovery key reached state");
        assert_eq!(app.console_state()["unattended"]["active"], true);
    }

    #[test]
    fn tpm2_requires_the_actual_probe_and_uses_the_reviewed_pin() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: true,
                secure_boot: true,
                kernel_lockdown: true,
                target_verified_boot: true,
            },
        );
        let mut value = recipe(false);
        value["protection"] = json!({"kind": "tpm2", "secret": PIN});
        let recipe = parse(&value);
        let mut sink =
            RecoveryDestination::File(tempfile::tempdir().unwrap().path().join("recovery-key"))
                .open()
                .unwrap();
        let mut probe = Probe(ProtectionReport {
            tpm2: true,
            pin_defensible: false,
            lockout_auth_set: false,
            shared_with_windows: false,
            target_verified_boot: true,
        });

        execute(&mut app, &recipe, &mut probe, &mut sink).unwrap();
        let installs = fake.installs.lock().unwrap().clone();
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].3.pin.as_str(), PIN);
        assert!(!installs[0].3.force_startup_passphrase);
        assert!(!installs[0].3.password_is_hash);
    }

    #[test]
    fn an_explicit_passphrase_plan_survives_a_tpm_verified_target() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: true,
                secure_boot: true,
                kernel_lockdown: true,
                target_verified_boot: true,
            },
        );
        let recipe = parse(&recipe(false));
        let mut sink =
            RecoveryDestination::File(tempfile::tempdir().unwrap().path().join("recovery-key"))
                .open()
                .unwrap();
        let mut probe = Probe(ProtectionReport {
            tpm2: true,
            pin_defensible: false,
            lockout_auth_set: false,
            shared_with_windows: false,
            target_verified_boot: true,
        });

        execute(&mut app, &recipe, &mut probe, &mut sink).unwrap();
        let installs = fake.installs.lock().unwrap().clone();
        assert_eq!(installs.len(), 1);
        assert_eq!(installs[0].3.pin.as_str(), DISK_SECRET);
        assert!(installs[0].3.force_startup_passphrase);
        assert_eq!(
            app.console_state()["account"]["secret_policy"],
            "passphrase"
        );
    }

    #[test]
    fn a_failed_hardware_probe_refuses_before_disk_mutation_and_leaves_the_wizard() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: true,
                secure_boot: true,
                kernel_lockdown: true,
                target_verified_boot: true,
            },
        );
        let mut value = recipe(false);
        value["protection"] = json!({"kind": "tpm2", "secret": PIN});
        let recipe = parse(&value);
        let mut sink =
            RecoveryDestination::File(tempfile::tempdir().unwrap().path().join("recovery-key"))
                .open()
                .unwrap();
        let error = execute(&mut app, &recipe, &mut Refuse, &mut sink).unwrap_err();
        assert!(error.contains("actual TPM2"));
        assert!(fake.installs.lock().unwrap().is_empty());

        app.note_unattended_error(error);
        let state = app.console_state();
        assert_eq!(state["unattended"]["active"], true);
        assert_eq!(
            state["unattended"]["error"],
            "the actual TPM2 did not answer"
        );
        assert_eq!(state["step"], "keyboard");
        let dump: Value =
            serde_json::from_str(&app.answer_console(r#"{"op":"state_dump"}"#)).unwrap();
        assert_eq!(
            dump["state"]["unattended"]["error"],
            "the actual TPM2 did not answer"
        );
        let listed: Value =
            serde_json::from_str(&app.answer_console(r#"{"op":"actions_enumerate"}"#)).unwrap();
        assert!(
            listed.get("answer").is_some(),
            "the recoverable console stays live: {listed}"
        );
    }

    #[test]
    fn a_changed_disk_identity_refuses_and_names_the_reviewed_target() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: false,
                secure_boot: false,
                kernel_lockdown: false,
                target_verified_boot: false,
            },
        );
        let mut value = recipe(false);
        value["disk"]["size_bytes"] = json!(41u64 * 1024 * 1024 * 1024);
        let recipe = parse(&value);
        let mut sink =
            RecoveryDestination::File(tempfile::tempdir().unwrap().path().join("recovery-key"))
                .open()
                .unwrap();
        let mut probe = Probe(ProtectionReport {
            tpm2: false,
            pin_defensible: false,
            lockout_auth_set: false,
            shared_with_windows: false,
            target_verified_boot: false,
        });

        let error = execute(&mut app, &recipe, &mut probe, &mut sink).unwrap_err();
        assert!(
            error.contains("no longer matches the actual block device"),
            "{error}"
        );
        assert!(error.contains("/dev/vdb"), "{error}");
        assert!(fake.installs.lock().unwrap().is_empty());

        app.note_unattended_error(error);
        assert_eq!(app.console_state()["step"], "disk");
        assert!(
            app.console_state()["unattended"]["error"]
                .as_str()
                .is_some()
        );
    }

    #[test]
    fn malformed_or_incomplete_recipes_are_refused_without_guessing() {
        for mutate in [
            |value: &mut serde_json::Value| value["uid"] = json!(1000),
            |value: &mut serde_json::Value| {
                value["account"]["password"] = json!("also a password");
            },
            |value: &mut serde_json::Value| {
                value["account"]["password_sha512_crypt"] = json!("not-a-hash");
                value["account"].as_object_mut().unwrap().remove("password");
            },
            |value: &mut serde_json::Value| value["data_source"] = json!("this_computer"),
            |value: &mut serde_json::Value| value["acknowledge_recovery_key"] = json!(false),
            |value: &mut serde_json::Value| value["disk"]["method"] = json!("alongside"),
            |value: &mut serde_json::Value| value["disk"]["partition_table"] = json!("dos"),
            |value: &mut serde_json::Value| value["protection"]["kind"] = json!("auto"),
            |value: &mut serde_json::Value| value["schema"] = json!(2),
        ] {
            let mut value = recipe(true);
            mutate(&mut value);
            assert!(
                Recipe::parse(serde_json::to_vec(&value).unwrap().as_slice()).is_err(),
                "accepted {value}"
            );
        }
    }

    #[test]
    fn invocation_reads_descriptors_and_never_takes_recipe_content_from_argv() {
        assert!(
            Invocation::from_args(Vec::<OsString>::new())
                .unwrap()
                .is_none()
        );
        assert!(
            Invocation::from_args([OsString::from("--shots"), OsString::from("x")])
                .unwrap()
                .is_none()
        );
        let invocation = Invocation::from_args([
            OsString::from("--unattended-fd"),
            OsString::from("3"),
            OsString::from("--unattended-recovery-fd"),
            OsString::from("4"),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(invocation.recipe, RecipeSource::Fd(3));
        assert_eq!(invocation.recovery, RecoveryDestination::Fd(4));
        for bad in [
            vec!["--unattended-fd", "2"],
            vec!["--unattended-fd", "3", "--unattended-file", "/x"],
            vec!["--unattended-file", "relative.json"],
            vec![
                "--unattended-fd",
                "3",
                "--unattended-recovery-fd",
                "4",
                "--unattended-recovery-file",
                "/x",
            ],
            vec!["--unattended-fd", "3", "--password", "secret"],
        ] {
            assert!(
                Invocation::from_args(bad.iter().map(OsString::from)).is_err(),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn private_recipe_input_is_no_follow_and_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recipe.json");
        std::fs::write(&path, serde_json::to_vec(&recipe(true)).unwrap()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let error = RecipeSource::File(path.clone())
            .read()
            .err()
            .expect("a shared recipe must be refused");
        assert!(error.contains("readable by another account"), "{error}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            RecipeSource::File(path.clone()).read().unwrap().schema,
            SCHEMA
        );

        let link = directory.path().join("recipe-link.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        let error = RecipeSource::File(link)
            .read()
            .err()
            .expect("a linked recipe must be refused");
        assert!(error.contains("without following a link"), "{error}");
    }

    #[test]
    fn recovery_output_refuses_to_overwrite_an_existing_path() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("recovery-key");
        std::fs::write(&path, b"sentinel").unwrap();
        let error = RecoveryDestination::File(path.clone())
            .open()
            .err()
            .expect("an existing recovery-key path must be refused");
        assert!(error.contains("could not be created privately"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), b"sentinel");
    }

    #[test]
    fn sha512_crypt_accepts_the_canonical_shape_and_refuses_lookalikes() {
        assert!(valid_sha512_crypt(&account_hash()));
        assert!(valid_sha512_crypt(&format!(
            "$6$rounds=5000$salt${}",
            "A".repeat(86)
        )));
        for bad in [
            "",
            "$6$salt$short",
            "$6$salt$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "$5$salt$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "$6$rounds=099$salt$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "$6$salt$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA!",
        ] {
            assert!(!valid_sha512_crypt(bad), "accepted {bad}");
        }
    }

    #[test]
    fn no_discovered_endpoint_with_a_terminal_binds_an_owned_fallback_and_stays_controllable() {
        let fake = Fake::succeeding();
        let mut app = build_app(
            &fake,
            Protection {
                tpm2: false,
                secure_boot: false,
                kernel_lockdown: false,
                target_verified_boot: false,
            },
        );
        let directory = tempfile::tempdir().unwrap();
        let door = start_console(
            crate::operation::PROVIDER,
            Vec::new(),
            true,
            Some(directory.path().to_path_buf()),
        )
        .unwrap();
        let socket = door.socket_path().expect("fallback socket").to_path_buf();
        assert!(socket.exists(), "the reported socket must be the bound one");
        assert!(
            door.instructions()
                .iter()
                .any(|line| line.contains(socket.to_string_lossy().as_ref())),
            "the continuation names the actual socket: {:?}",
            door.instructions()
        );

        // An invalid recipe is refused before any mutation, then the ordinary
        // wizard must remain reachable through the socket this process bound.
        let invalid = Recipe::parse(br#"{"schema":1,"account":{}}"#)
            .err()
            .expect("an incomplete recipe is refused");
        app.note_unattended_error(invalid);
        let mut client = UnixStream::connect(&socket).unwrap();
        writeln!(client, r#"{{"op":"state_dump"}}"#).unwrap();
        tilo_test_env::recv(
            "the wake a queued request gives the owner instead of leaving it to a poll",
            &door.wake,
        );
        assert_eq!(drain_console(&mut app, &door.console), 1);
        client
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut reply = String::new();
        BufReader::new(client).read_line(&mut reply).unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        assert!(reply.get("state").is_some(), "{reply}");
        assert_eq!(reply["state"]["unattended"]["active"], true);
        assert!(
            reply["state"]["unattended"]["error"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
    }

    #[test]
    fn a_console_bind_failure_is_a_clear_refusal_before_any_mutation() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let shared = directory.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let endpoint = console_control::Endpoint::Socket(shared.join("installer.sock"));
        let error = start_console(crate::operation::PROVIDER, vec![endpoint], false, None)
            .err()
            .expect("a shared socket directory must be refused, not served empty");
        assert!(error.contains("No console socket is available"), "{error}");
        assert!(
            error.contains("installer.sock"),
            "the failure names the path: {error}"
        );
        assert!(
            error.contains("closed to everyone else") || error.contains("Permission denied"),
            "the failure carries the bind reason: {error}"
        );
        assert!(
            !shared.join("installer.sock").exists(),
            "a refused bind must not leave a socket behind"
        );

        let error = start_console(crate::operation::PROVIDER, Vec::new(), false, None)
            .err()
            .expect("no endpoint and no terminal has no interactive continuation");
        assert!(error.contains("No console socket is available"), "{error}");
        assert!(error.contains("terminal"), "{error}");
    }

    #[test]
    fn recipe_errors_redact_every_secret_they_are_given() {
        let plaintext = parse(&recipe(false));
        let message = format!(
            "password {} disk {}",
            "ordinary typed password", DISK_SECRET
        );
        let redacted = plaintext.redact(&message);
        assert!(!redacted.contains("ordinary typed password"));
        assert!(!redacted.contains(DISK_SECRET));
        assert!(redacted.contains("[redacted]"));

        let hashed = parse(&recipe(true));
        let message = format!("hash {} disk {}", account_hash(), DISK_SECRET);
        let redacted = hashed.redact(&message);
        assert!(!redacted.contains(&account_hash()));
        assert!(!redacted.contains(DISK_SECRET));
        assert!(redacted.contains("[redacted]"));
    }
}
