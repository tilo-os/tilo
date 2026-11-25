//! Enrolling the sibling factor on a volume (`tilo-disk-encrypt enrol
//! --sibling`), and removing it.
//!
//! 1. Draw the device half, the sibling half and a boot-only key pair.
//! 2. Add a key slot opened by the key the two halves derive, authorised by an
//!    existing secret of the volume.
//! 3. Hand the boot key's request and the sibling half to this machine's
//!    daemon ([`Registrar`]). The person's root certifies the boot key: on
//!    this machine where it keeps the root, and otherwise at one of their
//!    devices that does, where they are asked which computer wants it. The
//!    daemon seals the sibling half to the person's secret-sync key, publishes
//!    the enrolment to their devices, and answers with the certificate and the
//!    devices that carry the half. A no, or no device of theirs that keeps the
//!    root within reach, is said in words the person can act on.
//! 4. Store the token: the certificate, those devices, and the device half,
//!    boot key and chosen unlock networks, sealed to the TPM where there is
//!    one and otherwise in the clear.
//!
//! The sibling half is wiped here once it is handed over. A failure after the
//! slot was added removes the slot again, so a half-enrolled volume never
//! remains.
//!
//! Enrolling again with `--replace-networks` keeps the slot, the certificate
//! and the siblings, and only chooses the unlock networks anew: the material
//! is unsealed, its networks replaced, and it is sealed the same way again.
//!
//! What is enrolled is described for Settings under [`STATE_DIR`], without
//! secrets but readable only by root: it names the networks the owner trusts
//! to unlock this machine. Settings reads it through polkit
//! (`tilo-auth sibling volumes`).

use super::material::{self, Material};
use super::token::{Sibling, TOKEN_TYPE, TOKEN_VERSION, Token};
use crate::luks::{self, Authority};
use crate::tpm::{self, Sealing};
use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tilo_api::daemon::DaemonPlace;
use tilo_device_identity::login::daemon::DeviceRefusal;
use tilo_rollback::exec::Runner;
use tilo_rollback::policy::Workspace;
use tilo_unlock_protocol::{EnrolRequest, EnrolResponse, Half, SlotId, derive_slot_key};
use tilo_user_identity::{DeviceCapabilities, verify_device_cert};

/// Where the enrolment is described for Settings, without secrets.
pub const STATE_DIR: &str = "/var/lib/tilo/sibling-unlock";

/// `tilo-auth sibling enrol`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The LUKS2 partition.
    pub device: PathBuf,
    /// A key file holding an existing passphrase of the volume; without one
    /// it is asked for on the terminal (the recovery key will do).
    #[arg(long)]
    pub key_file: Option<PathBuf>,
    /// A `NetworkManager` connection early boot may join to reach your other
    /// devices (repeatable). Wired links need none. Its saved password goes
    /// with it: sealed to the TPM where there is one, otherwise readable by
    /// anyone holding the disk.
    #[arg(long = "network")]
    pub networks: Vec<String>,
    /// Keep the volume's sibling unlock and only choose its unlock networks
    /// again: the `--network` list replaces the earlier one, and none leaves
    /// wired links only. Needs no passphrase and no signed-in session.
    #[arg(long)]
    pub replace_networks: bool,
    /// The account whose daemon enrols the boot key, on a machine whose
    /// daemon runs as that person's own service. Without it this process's
    /// own account's daemon is asked.
    #[arg(long)]
    pub session_uid: Option<u32>,
    /// Refuse enrollment unless the device factor can be sealed to the TPM.
    #[arg(long)]
    pub require_tpm: bool,
    /// Accept this machine's current boot chain, so the owner's other devices
    /// release its sibling half without asking them: an unattended unlock,
    /// for a server that reboots with nobody there. Needs a TPM, and covers
    /// the chain measured now, so it is chosen again after an OS update that
    /// changes PCR 4.
    #[arg(long)]
    pub unattended: bool,
}

/// Has the person's root certify a boot key, and the enrolment published to their devices.
pub trait Registrar {
    /// Certifies `request`'s boot key, seals `half` to the person's secret-sync key and
    /// publishes the enrolment: the certificate and the devices that carry the half.
    fn register(&self, request: &EnrolRequest, half: &Half) -> Result<EnrolResponse, NotEnrolled>;
}

/// Why the person's root did not certify this device's boot key: each in the words this device's
/// daemon gives it, in the person's language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotEnrolled {
    /// The person said no where they were asked: the disk stays as locked as it was.
    Declined(String),
    /// This device keeps no root of theirs and none of their devices that keeps it is reachable
    /// to ask them.
    NoRootDevice(String),
    /// Not enrolled, with a reason for the person.
    Failed(String),
}

impl std::fmt::Display for NotEnrolled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Declined(words) | Self::NoRootDevice(words) | Self::Failed(words) => {
                formatter.write_str(words)
            }
        }
    }
}

impl std::error::Error for NotEnrolled {}

impl From<DeviceRefusal> for NotEnrolled {
    fn from(refusal: DeviceRefusal) -> Self {
        match refusal.code.as_str() {
            tilo_device_identity::login::UNLOCK_DECLINED => Self::Declined(refusal.message),
            tilo_device_identity::login::NO_ROOT_DEVICE => Self::NoRootDevice(refusal.message),
            _ => Self::Failed(refusal.message),
        }
    }
}

/// This machine's daemon, which alone keeps the device key: it has the person's root certify the
/// boot key ([`SET_UP_UNLOCK`]), here where it keeps it and otherwise at a device of theirs that does,
/// which asks them, and publishes the enrolment.
///
/// [`SET_UP_UNLOCK`]: tilo_device_identity::login::SET_UP_UNLOCK
pub struct DaemonRegistrar(DaemonPlace);

impl DaemonRegistrar {
    /// The daemon `daemon`.
    #[must_use]
    pub const fn new(daemon: DaemonPlace) -> Self {
        Self(daemon)
    }

    /// The daemon `session_uid`'s account runs, or this account's.
    ///
    /// # Errors
    /// No daemon of that account can be named.
    pub fn find(session_uid: Option<u32>) -> Result<Self> {
        local_daemon(session_uid).map(Self)
    }
}

impl Registrar for DaemonRegistrar {
    fn register(&self, request: &EnrolRequest, half: &Half) -> Result<EnrolResponse, NotEnrolled> {
        let request = hex::encode(
            postcard::to_allocvec(request)
                .map_err(|error| NotEnrolled::Failed(format!("the request: {error}")))?,
        );
        let half = zeroize::Zeroizing::new(hex::encode(half.expose()));
        let answer = self.0.actions().ask_followed(
            tilo_device_identity::login::SET_UP_UNLOCK,
            serde_json::json!({ "request": request, "half": half.as_str() }),
        )?;
        answer
            .get("enrolled")
            .and_then(serde_json::Value::as_str)
            .and_then(|enrolled| hex::decode(enrolled).ok())
            .and_then(|enrolled| postcard::from_bytes(&enrolled).ok())
            .ok_or_else(|| {
                NotEnrolled::Failed(format!("the device service answered no enrolment: {answer}"))
            })
    }
}

/// Where a login account keeps its identity records, relative to its home.
const IDENTITY_DIR_IN_HOME: &str = ".config/tilo/identity";

/// The daemon that enrols the boot: this account's, or the one `session_uid` names on
/// a machine whose daemon runs as the system unit.
fn local_daemon(session_uid: Option<u32>) -> Result<DaemonPlace> {
    let ours = rustix::process::geteuid().as_raw();
    match session_uid.filter(|uid| *uid != ours) {
        None => tilo_api::daemon::this_computer().map_err(|error| anyhow!("{error}")),
        Some(uid) => {
            let sockets = PathBuf::from(format!("/run/user/{uid}/tilo"));
            let dir = home_of(uid)?.join(IDENTITY_DIR_IN_HOME);
            Ok(DaemonPlace::at(&sockets, &dir))
        }
    }
}

/// The home directory of account `uid`.
fn home_of(uid: u32) -> Result<PathBuf> {
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // The largest home directory Linux accepts; getpwuid_r reports what it needs.
    let mut buffer = vec![0_u8; 4096];
    // SAFETY: the buffer and the entry pointers are valid for the call, and the
    // returned entry points into `buffer`, which outlives the reads below.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() {
        bail!("this device has no account {uid}");
    }
    // SAFETY: a non-null result points at the initialized `entry`.
    let entry = unsafe { entry.assume_init() };
    // SAFETY: the home pointer is a NUL-terminated string inside `buffer`.
    let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    let home = home.to_str().context("this account's home is not UTF-8")?;
    Ok(PathBuf::from(home))
}

/// What Settings shows about an enrolment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub keyslot: u32,
    /// `tpm2`, or `cleartext`: the device half and the unlock networks'
    /// passwords are readable by anyone holding the disk.
    pub sealing: Sealing,
    pub networks: Vec<String>,
    /// The devices asked at boot, by label.
    pub siblings: Vec<String>,
}

/// The file `NetworkManager` keeps the saved connection called `name` in.
fn keyfile_of(name: &str) -> Result<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("no async runtime")?;
    let profiles = runtime
        .block_on(async { tilo_nm::NmClient::system().await?.profiles().await })
        .context("could not ask NetworkManager for its saved connections")?;
    profiles
        .into_iter()
        .find(|profile| profile.settings.id() == name)
        .map(|profile| profile.filename)
        .filter(|file| !file.is_empty())
        .ok_or_else(|| anyhow!("no saved connection is named {name:?}"))
}

/// Reads a `NetworkManager` connection's keyfile by name, secrets included.
fn network(name: &str) -> Result<material::Network> {
    if !material::valid_network_id(name) {
        bail!("{name:?} is not a usable connection name");
    }
    let file = keyfile_of(name)?;
    let keyfile = std::fs::read_to_string(&file)
        .with_context(|| format!("could not read the saved connection {file}"))?;
    let keyfile = for_early_boot(&keyfile, |interface| {
        std::fs::read_to_string(format!("/sys/class/net/{interface}/address"))
            .ok()
            .map(|mac| mac.trim().to_owned())
    });
    let secret_elsewhere = keyfile.lines().any(|line| {
        let line = line.trim();
        (line.starts_with("psk-flags=") || line.starts_with("password-flags="))
            && !line.ends_with("=0")
    });
    if secret_elsewhere {
        bail!(
            "the password of {name:?} is kept in your session, not with the connection; save it for all users first"
        );
    }
    Ok(material::Network {
        id: name.to_owned(),
        keyfile,
    })
}

/// A saved connection as early boot can use it. A profile limited to one
/// user's session cannot come up before login, so its `permissions=` go.
/// Interface names may differ in the initramfs, so a profile tied to one
/// interface is tied to that interface's MAC address instead.
fn for_early_boot(keyfile: &str, mac_of: impl Fn(&str) -> Option<String>) -> String {
    let mut mac = None;
    let mut kept: Vec<String> = Vec::new();
    for line in keyfile.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("permissions=") {
            continue;
        }
        if let Some(interface) = trimmed.strip_prefix("interface-name=") {
            mac = mac_of(interface.trim());
            continue;
        }
        kept.push(line.to_owned());
    }
    let has_mac = kept
        .iter()
        .any(|line| line.trim_start().starts_with("mac-address="));
    if let (Some(mac), false) = (mac, has_mac) {
        let kind = kept
            .iter()
            .find_map(|line| line.trim().strip_prefix("type="))
            .map_or("ethernet", |kind| match kind {
                "802-11-wireless" => "wifi",
                "802-3-ethernet" => "ethernet",
                other => other,
            })
            .to_owned();
        let header = format!("[{kind}]");
        if let Some(at) = kept.iter().position(|line| line.trim() == header) {
            kept.insert(at.saturating_add(1), format!("mac-address={mac}"));
        } else {
            kept.push(header);
            kept.push(format!("mac-address={mac}"));
        }
    }
    kept.iter().fold(String::new(), |mut out, line| {
        out.push_str(line);
        out.push('\n');
        out
    })
}

/// This machine as an enrolment uses it: the runner that reaches its
/// cryptsetup and TPM, and whether it has a TPM at all.
#[derive(Clone, Copy)]
pub struct Machine<'a> {
    pub runner: &'a dyn Runner,
    pub has_tpm: bool,
}

impl Machine<'_> {
    /// Enrolment needs the device to be able to attest, so a request for
    /// unattended unlock on a machine with no TPM fails instead of silently
    /// asking the owner for every boot.
    fn require(&self, unattended: bool) -> Result<()> {
        if unattended && !self.has_tpm {
            bail!("unattended unlock needs a TPM to attest this machine's boot chain");
        }
        Ok(())
    }

    fn sealing(&self) -> Sealing {
        if self.has_tpm {
            Sealing::Tpm2
        } else {
            Sealing::Cleartext
        }
    }
}

/// An existing passphrase of the volume: the key file's, or one asked for on
/// the terminal.
pub fn existing_passphrase(
    device: &Path,
    key_file: Option<&Path>,
) -> Result<zeroize::Zeroizing<Vec<u8>>> {
    if let Some(file) = key_file {
        return std::fs::read(file)
            .map(zeroize::Zeroizing::new)
            .with_context(|| format!("could not read {}", file.display()));
    }
    let output = std::process::Command::new("systemd-ask-password")
        .arg("--timeout=0")
        .arg(format!("An existing passphrase of {}:", device.display()))
        .stdin(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("could not ask for an existing passphrase")?;
    let mut answer = zeroize::Zeroizing::new(output.stdout);
    if !output.status.success() {
        bail!("no passphrase was given");
    }
    if answer.last() == Some(&b'\n') {
        answer.pop();
    }
    Ok(answer)
}

/// Enrols the sibling factor.
///
/// # Errors
/// When any step fails; the slot is removed again.
pub fn run(args: &Args, machine: Machine<'_>) -> Result<()> {
    let sealing = enrollment_sealing(machine.sealing(), args.require_tpm)?;
    machine.require(args.unattended)?;
    let enrolled = luks::sibling(machine.runner, &args.device)?.is_some();
    match (enrolled, args.replace_networks) {
        (true, false) => bail!(
            "{} already has a sibling unlock; pass --replace-networks to choose its networks again, or remove it first",
            args.device.display()
        ),
        (false, true) => bail!(
            "{} has no sibling unlock whose networks could be replaced",
            args.device.display()
        ),
        _ => {}
    }
    let networks = args
        .networks
        .iter()
        .map(|name| network(name))
        .collect::<Result<Vec<_>>>()?;
    if args.replace_networks {
        return replace_networks(machine.runner, &args.device, networks);
    }
    let registrar = DaemonRegistrar::find(args.session_uid)?;
    let existing = existing_passphrase(&args.device, args.key_file.as_deref())?;
    enrol(
        machine,
        &args.device,
        &existing,
        networks,
        &registrar,
        sealing,
        args.unattended,
    )
}

fn enrollment_sealing(available: Sealing, require_tpm: bool) -> Result<Sealing> {
    if require_tpm && available != Sealing::Tpm2 {
        bail!("TPM sealing is required for this enrollment");
    }
    Ok(available)
}

/// The enrolment itself, with the registrar as a boundary. `existing` is a
/// passphrase the volume opens with, which authorises the new slot.
///
/// # Errors
/// As [`run`].
pub fn enrol(
    machine: Machine<'_>,
    device: &Path,
    existing: &[u8],
    networks: Vec<material::Network>,
    registrar: &impl Registrar,
    sealing: Sealing,
    unattended: bool,
) -> Result<()> {
    machine.require(unattended)?;
    let runner = machine.runner;
    let dir = crate::scratch()?;
    let ws = Workspace::new(dir.path());
    let volume_uuid = luks::uuid(runner, device)?;
    let device_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let sibling_half = Half::generate().map_err(|error| anyhow!("{error}"))?;
    let boot_key = SigningKey::from_bytes(
        Half::generate()
            .map_err(|error| anyhow!("{error}"))?
            .expose(),
    );
    let passphrase = derive_slot_key(&device_half, &sibling_half, &volume_uuid);
    let keyslot = luks::add_slot(
        runner,
        &ws,
        device,
        &Authority::Passphrase(existing),
        passphrase.as_bytes(),
    )?;
    drop(passphrase);
    // Everything after the slot exists can still fail, and the slot is
    // removed again when it does. That includes the TPM: an unattended
    // enrolment whose attestation key cannot be made fails rather than
    // leaving a machine that silently falls back to a prompt.
    let finish = || -> Result<Token> {
        let (attestation, attestation_key, unattended_chain) = if machine.has_tpm {
            let (attestation, point) = tpm::create_attestation_key(runner)?;
            let chain = if unattended {
                Some(tpm::current_chain(runner)?)
            } else {
                None
            };
            (Some(attestation), Some(point), chain)
        } else {
            (None, None, None)
        };
        let slot = SlotId {
            volume_uuid,
            keyslot: u16::try_from(keyslot).context("key slot out of range")?,
        };
        let request = EnrolRequest::new(&boot_key, slot, attestation_key, unattended_chain)
            .map_err(|error| anyhow!("{error}"))?;
        let response = registrar.register(&request, &sibling_half)?;
        drop(sibling_half);
        let cert = &response.cert;
        verify_device_cert(cert).map_err(|error| anyhow!("the issued certificate: {error}"))?;
        if cert.device_public_key != boot_key.verifying_key().to_bytes() {
            bail!("the issued certificate is for another boot key");
        }
        if cert.capabilities != DeviceCapabilities::nothing() {
            bail!("the issued certificate grants the boot key more than nothing");
        }
        if response.siblings.is_empty() {
            bail!("none of your other devices holds the sibling half");
        }
        let material = Material::new(&device_half, &boot_key, networks, attestation);
        let token = Token {
            kind: TOKEN_TYPE.into(),
            keyslots: vec![keyslot.to_string()],
            tilo_version: TOKEN_VERSION,
            cert: Token::encode_cert(cert)?,
            siblings: response.siblings.iter().map(Sibling::from).collect(),
            sealing,
            material: tpm::seal_material(runner, sealing, &material.to_json()?)?,
        };
        token.validate()?;
        luks::write_sibling(runner, &ws, device, &token)?;
        Ok(token)
    };
    match finish() {
        Ok(token) => {
            write_status(&status_of(runner, &token)?, &volume_uuid);
            Ok(())
        }
        Err(error) => {
            let _ = luks::kill_slot(runner, &ws, device, keyslot, Some(existing));
            Err(error)
        }
    }
}

/// Chooses the unlock networks of an enrolled volume again, leaving its slot,
/// certificate and siblings as they are. The material is sealed the way it
/// was: a volume enrolled in the clear stays in the clear.
pub fn replace_networks(
    runner: &dyn Runner,
    device: &Path,
    networks: Vec<material::Network>,
) -> Result<()> {
    let mut token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    token.validate()?;
    let mut material = Material::from_json(&tpm::unseal_material(
        runner,
        token.sealing,
        &token.material,
    )?)?;
    material.networks = networks;
    token.material = tpm::seal_material(runner, token.sealing, &material.to_json()?)?;
    token.validate()?;
    let dir = crate::scratch()?;
    luks::write_sibling(runner, &Workspace::new(dir.path()), device, &token)?;
    write_status(&status_of(runner, &token)?, &luks::uuid(runner, device)?);
    Ok(())
}

fn status_of(runner: &dyn Runner, token: &Token) -> Result<Status> {
    let material = Material::from_json(&tpm::unseal_material(
        runner,
        token.sealing,
        &token.material,
    )?)?;
    Ok(Status {
        keyslot: token.keyslot()?,
        sealing: token.sealing,
        networks: material
            .networks
            .iter()
            .map(|network| network.id.clone())
            .collect(),
        siblings: token
            .siblings
            .iter()
            .map(|sibling| {
                if sibling.label.is_empty() {
                    sibling.device.clone()
                } else {
                    sibling.label.clone()
                }
            })
            .collect(),
    })
}

fn write_status(status: &Status, volume_uuid: &[u8; 16]) {
    let _ = write_status_in(Path::new(STATE_DIR), status, volume_uuid);
}

/// Writes the description root-only, replacing an earlier one whole.
fn write_status_in(dir: &Path, status: &Status, volume_uuid: &[u8; 16]) -> Result<()> {
    let path = dir.join(format!("{}.json", hex::encode(volume_uuid)));
    tilo_files::native::atomic::write_private(&path, &serde_json::to_vec_pretty(status)?)?;
    Ok(())
}

/// One volume enrolled on this machine, as `tilo-auth sibling volumes`
/// prints it for Settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Enrolled {
    /// The LUKS UUID, as `/dev/disk/by-uuid` names the volume.
    pub volume: String,
    /// Where `enrol --replace-networks` finds it.
    pub device: PathBuf,
    #[serde(flatten)]
    pub status: Status,
}

/// Every enrolment described under [`STATE_DIR`].
pub fn enrolled() -> Result<Vec<Enrolled>> {
    enrolled_in(Path::new(STATE_DIR))
}

fn enrolled_in(dir: &Path) -> Result<Vec<Enrolled>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error).with_context(|| format!("could not read {}", dir.display()));
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let Some(hex_uuid) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".json"))
        else {
            continue;
        };
        let Some(volume) = dashed_uuid(hex_uuid) else {
            continue;
        };
        let status: Status = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("{} is malformed", path.display()))?;
        out.push(Enrolled {
            device: Path::new("/dev/disk/by-uuid").join(&volume),
            volume,
            status,
        });
    }
    out.sort_by(|a, b| a.volume.cmp(&b.volume));
    Ok(out)
}

/// `0123…` (32 hex digits) as `01234567-89ab-cdef-0123-456789abcdef`.
fn dashed_uuid(hex_uuid: &str) -> Option<String> {
    let bytes = hex::decode(hex_uuid)
        .ok()
        .filter(|bytes| bytes.len() == 16)?;
    let part = |range: std::ops::Range<usize>| bytes.get(range).map(hex::encode);
    Some(format!(
        "{}-{}-{}-{}-{}",
        part(0..4)?,
        part(4..6)?,
        part(6..8)?,
        part(8..10)?,
        part(10..16)?
    ))
}

/// What is enrolled on `device`.
pub fn status(runner: &dyn Runner, device: &Path) -> Result<Status> {
    let token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    token.validate()?;
    status_of(runner, &token)
}

/// Removes the sibling slot and its token, with a passphrase of the volume
/// where the caller has one.
pub fn remove(runner: &dyn Runner, device: &Path, existing: Option<&[u8]>) -> Result<()> {
    let token = luks::sibling(runner, device)?
        .ok_or_else(|| anyhow!("{} has no sibling unlock", device.display()))?;
    luks::remove_sibling(runner, device)?;
    let dir = crate::scratch()?;
    luks::kill_slot(
        runner,
        &Workspace::new(dir.path()),
        device,
        token.keyslot()?,
        existing,
    )?;
    let _ = std::fs::remove_file(
        Path::new(STATE_DIR).join(format!("{}.json", hex::encode(luks::uuid(runner, device)?))),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_description_is_root_only_and_lists_back() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("sibling-unlock");
        let status = Status {
            keyslot: 2,
            sealing: Sealing::Cleartext,
            networks: vec!["home".into()],
            siblings: vec!["laptop".into()],
        };
        let uuid = [0xab_u8; 16];
        write_status_in(&dir, &status, &uuid).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        let file = dir.join(format!("{}.json", hex::encode(uuid)));
        assert_eq!(mode(&file), 0o600);

        let replaced = Status {
            networks: Vec::new(),
            ..status
        };
        write_status_in(&dir, &replaced, &uuid).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let listed = enrolled_in(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].volume, "abababab-abab-abab-abab-abababababab");
        assert_eq!(
            listed[0].device,
            Path::new("/dev/disk/by-uuid/abababab-abab-abab-abab-abababababab")
        );
        assert_eq!(listed[0].status, replaced);

        let json = serde_json::to_value(&listed[0]).unwrap();
        assert_eq!(json["sealing"], "cleartext");
        assert_eq!(json["networks"], serde_json::json!([]));
        assert!(enrolled_in(&root.path().join("absent")).unwrap().is_empty());
    }

    #[test]
    fn a_saved_connection_is_tied_to_its_mac_for_early_boot() {
        let saved = "[connection]\nid=lan\ntype=ethernet\ninterface-name=enp0s4\npermissions=user:tester;\n\n[ethernet]\n\n[ipv4]\naddress1=192.168.231.11/24\nmethod=manual\n";
        let adapted = for_early_boot(saved, |interface| {
            (interface == "enp0s4").then(|| "52:54:00:23:83:0b".to_owned())
        });
        assert!(
            !adapted.contains("interface-name") && !adapted.contains("permissions"),
            "{adapted}"
        );
        assert!(
            adapted.contains("[ethernet]\nmac-address=52:54:00:23:83:0b\n"),
            "{adapted}"
        );
        assert!(adapted.contains("address1=192.168.231.11/24"));

        let wifi =
            "[connection]\nid=home\ntype=wifi\ninterface-name=wlan0\n[wifi-security]\npsk=x\n";
        let adapted = for_early_boot(wifi, |_| Some("aa:bb:cc:dd:ee:ff".to_owned()));
        assert!(
            adapted.ends_with("[wifi]\nmac-address=aa:bb:cc:dd:ee:ff\n"),
            "{adapted}"
        );
    }
}
