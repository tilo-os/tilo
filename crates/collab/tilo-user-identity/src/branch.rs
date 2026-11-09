//! A machine that came up from another machine's disk: a branch.
//!
//! A person sets up one machine and branches others off it, copying the disk.
//! Everything on the disk comes along, the device key too, and two machines
//! holding one device key are one Iroh endpoint, one CRDT author and one
//! filesystem subject: they collide. A branch must come up as a new device
//! of the same person instead, with every secret it holds still usable.
//!
//! # Which machine the key belongs to
//!
//! Beside the device key this module keeps the machine it belongs to
//! ([`machine_record`]): the firmware's system UUID
//! (`/sys/class/dmi/id/product_uuid`), which a virtual machine host sets per
//! machine and keeps across that machine's reboots. `/etc/machine-id` is no
//! such record: it is on the disk, so a copy carries it.
//!
//! [`settle`] runs at boot, before anything uses the key:
//!
//! - A machine with no firmware UUID is never re-keyed automatically.
//! - A key with no machine on record (an install from before the record, or
//!   the first boot) gets the current machine recorded, and nothing else.
//! - The recorded machine is this one: nothing to do.
//! - Another machine is on record: this disk is a branch, and it re-keys.
//!
//! # Re-keying
//!
//! The re-key is a journal ([`journal_path`], private to root) written before
//! anything changes, holding the keys the machine's stores may be sealed
//! under, the new key and the machine it is for. Every step after it can run
//! again with the same result, so a boot interrupted anywhere resumes from
//! the journal and never strands a secret:
//!
//! 1. every device-sealed store is re-sealed from the old key to the new one
//!    ([`SealedStore`]); a store already under the new key, or under neither
//!    (another identity's), is left as it is;
//! 2. the new key replaces the device key;
//! 3. the device is certified under its person ([`Certification`]);
//! 4. the machine-wide names a copy shares are renewed
//!    ([`MachineIdentity`]: the machine id, SSH host keys);
//! 5. the machine is recorded and the journal, with the old key, removed.
//!
//! A store that cannot be re-sealed keeps the journal, and with it the old
//! key, until a later boot re-seals it; the device acts with its new key
//! meanwhile. The old device is never revoked: the machine it was copied from
//! still uses it.
//!
//! # Certification
//!
//! The new key needs a certificate from the person's root. This module adds
//! no trust to issue one:
//!
//! - When this machine holds the person's root in the system identity tree
//!   (`user-identity/privkey.bin`), it signs the new device's certificate
//!   itself, granting exactly what the source device's certificate granted
//!   and nothing more. A machine holding the root could sign any certificate
//!   for that person already.
//! - Otherwise the certificate stays pending ([`pending_label_path`] names
//!   the device it will be): a person whose root is in their own identity
//!   record has it issued at their next sign-in, as on any of their devices
//!   without one; a device that joined without the root is paired again,
//!   which one approval on a device holding the root completes. The secrets
//!   stay usable in the meantime: the stores are sealed to the device key,
//!   not to its certificate.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub use ed25519_dalek::SigningKey;
use getrandom::{SysRng, rand_core::UnwrapErr};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::cert::{DeviceCert, device_name, sign_device_cert, verify_device_cert_at};
use crate::persist::{self, IdentityPaths, PersistError};

/// Where Linux reports the firmware's system UUID. Readable by root only.
pub const PRODUCT_UUID: &str = "/sys/class/dmi/id/product_uuid";

const JOURNAL_VERSION: u8 = 1;
const MAX_JOURNAL_BYTES: usize = 4096;
/// The most keys a store may still be sealed under: the machine's own, and
/// those of the machines it was copied from mid-branch.
const MAX_FROM_KEYS: usize = 8;
/// A device label's bound where the root signs it on the device itself.
const MAX_LABEL_BYTES: usize = 64;
const BRANCH_WORD: &str = " branch ";
const PRIVATE_MODE: u32 = 0o600;
const RECORD_MODE: u32 = 0o640;

/// A machine's firmware identity: the SMBIOS system UUID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Machine([u8; 16]);

impl Machine {
    /// The machine `text` names, as `product_uuid` prints it. `None` for
    /// anything else, and for the all-zero and all-one placeholders firmware
    /// reports when it has no UUID to give.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let hex: String = text.chars().filter(|c| *c != '-').collect();
        if hex.len() != 32 || text.len() > 36 {
            return None;
        }
        let bytes: [u8; 16] = hex::decode(hex).ok()?.try_into().ok()?;
        (bytes != [0; 16] && bytes != [0xff; 16]).then_some(Self(bytes))
    }

    /// This machine's identity, read from `path` (normally [`PRODUCT_UUID`]).
    /// `None` when the platform reports none.
    ///
    /// # Errors
    ///
    /// When the file exists but cannot be read.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether `other` is this machine. Firmware has reported the first
    /// three fields in either byte order across SMBIOS versions, so an update
    /// that flips them is the same machine, not a branch.
    #[must_use]
    pub fn is(&self, other: &Self) -> bool {
        self.0 == other.0 || self.0 == other.swapped()
    }

    fn swapped(&self) -> [u8; 16] {
        let mut bytes = self.0;
        bytes[0..4].reverse();
        bytes[4..6].reverse();
        bytes[6..8].reverse();
        bytes
    }
}

/// The record of the machine the device key belongs to, beside the key.
#[must_use]
pub fn machine_record(paths: &IdentityPaths) -> PathBuf {
    paths.device_privkey.with_file_name("machine.bin")
}

/// The re-key journal, beside the device key and private to its owner.
#[must_use]
pub fn journal_path(paths: &IdentityPaths) -> PathBuf {
    paths.device_privkey.with_file_name("branch.bin")
}

/// The name a branched device's certificate carries once one is issued,
/// public beside the certificates while it is pending.
#[must_use]
pub fn pending_label_path(paths: &IdentityPaths) -> PathBuf {
    paths.device_cert.with_file_name("branch-label")
}

/// The name a branched device waits to be certified with, if it waits.
///
/// # Errors
///
/// When the file exists but cannot be read.
pub fn pending_label(paths: &IdentityPaths) -> io::Result<Option<String>> {
    match persist::read_regular_bounded(&pending_label_path(paths), MAX_LABEL_BYTES) {
        Ok(bytes) => Ok(String::from_utf8(bytes)
            .ok()
            .map(|label| label.trim().to_owned())
            .filter(|label| valid_label(label))),
        Err(PersistError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io::Error::other(error.to_string())),
    }
}

/// Forget the pending name once the device's certificate carries it.
///
/// # Errors
///
/// When the file exists and cannot be removed.
pub fn clear_pending_label(paths: &IdentityPaths) -> io::Result<()> {
    remove_if_present(&pending_label_path(paths))
}

/// Record that the device key belongs to `machine`, if nothing is recorded.
/// The first boot calls this right after it creates the key.
///
/// # Errors
///
/// When the record cannot be read or written.
pub fn record_machine(paths: &IdentityPaths, machine: &Machine) -> Result<bool, PersistError> {
    if read_record(paths)?.is_some() {
        return Ok(false);
    }
    write_record(paths, machine)?;
    Ok(true)
}

/// A store sealed under a key the device key derives: re-sealed when the
/// device key changes.
pub trait SealedStore {
    /// What the store is, for the boot log.
    fn name(&self) -> String;

    /// Re-seal from whichever of `from` the store is under to `to`.
    ///
    /// Must be idempotent: a store already under `to`, missing, or under none
    /// of these keys (another identity's) is left untouched and says so.
    ///
    /// # Errors
    ///
    /// Why the store could not be re-sealed now. It is tried again at the
    /// next boot, with the same keys.
    fn reseal(&self, from: &[SigningKey], to: &SigningKey) -> Result<Resealed, String>;
}

/// What re-sealing a store did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resealed {
    /// It was under an old key and is under the new one now.
    Moved,
    /// It was under the new key already: a resumed re-key.
    AlreadyNew,
    /// There is no such store on this machine.
    Absent,
    /// It is under none of the keys: another identity's, left as it is.
    Foreign,
    /// It named the old device itself, so no re-seal could make it this
    /// device's: it was dropped, and is rebuilt from the device's peers.
    Reset,
}

impl Resealed {
    /// The word a re-sealer in another process answers with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Moved => "moved",
            Self::AlreadyNew => "already-new",
            Self::Absent => "absent",
            Self::Foreign => "foreign",
            Self::Reset => "reset",
        }
    }

    /// The outcome `word` names.
    #[must_use]
    pub fn parse(word: &str) -> Option<Self> {
        [
            Self::Moved,
            Self::AlreadyNew,
            Self::Absent,
            Self::Foreign,
            Self::Reset,
        ]
        .into_iter()
        .find(|outcome| outcome.as_str() == word.trim())
    }
}

/// The keys a re-sealer running as another account is handed on its
/// standard input: a count, the keys a store may be under, then the new one.
#[must_use]
pub fn hand_over(from: &[SigningKey], to: &SigningKey) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        from.len()
            .saturating_add(1)
            .saturating_mul(32)
            .saturating_add(1),
    ));
    bytes.push(u8::try_from(from.len()).unwrap_or(u8::MAX));
    for key in from.iter().take(usize::from(u8::MAX)).chain([to]) {
        bytes.extend_from_slice(Zeroizing::new(key.to_bytes()).as_ref());
    }
    bytes
}

/// The keys [`hand_over`] wrote: those a store may be under, and the new one.
///
/// # Errors
///
/// When `bytes` is not exactly what [`hand_over`] writes.
pub fn take_over(bytes: &[u8]) -> io::Result<(Vec<SigningKey>, SigningKey)> {
    let malformed = || io::Error::new(io::ErrorKind::InvalidData, "malformed device keys");
    let (&count, keys) = bytes.split_first().ok_or_else(malformed)?;
    let count = usize::from(count);
    if count == 0
        || count > MAX_FROM_KEYS
        || Some(keys.len()) != count.checked_add(1).and_then(|n| n.checked_mul(32))
    {
        return Err(malformed());
    }
    let mut keys: Vec<SigningKey> = keys
        .chunks_exact(32)
        .map(|chunk| {
            let mut seed = Zeroizing::new([0_u8; 32]);
            seed.copy_from_slice(chunk);
            SigningKey::from_bytes(&seed)
        })
        .collect();
    let to = keys.pop().ok_or_else(malformed)?;
    Ok((keys, to))
}

/// The names a copy of the disk shares with its source beyond the device
/// key: renewed once per branch.
pub trait MachineIdentity {
    /// Give this machine names of its own.
    ///
    /// # Errors
    ///
    /// When they cannot be renewed; the re-key resumes at the next boot.
    fn renew(&self) -> io::Result<()>;
}

/// A step of a re-key, as [`Branch::after`] is told about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Journal,
    Stores,
    Key,
    Certificate,
    Machine,
    Record,
}

/// What a re-key works on.
pub struct Branch<'a> {
    pub stores: Vec<&'a dyn SealedStore>,
    pub machine: Option<&'a dyn MachineIdentity>,
    pub now_ns: u64,
    /// Told when each stage is on disk. An error stops the re-key there, as
    /// a crash would; the next [`settle`] resumes it.
    pub after: &'a dyn Fn(Stage) -> io::Result<()>,
}

/// How the device's new key got its certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Certification {
    /// The person's root on this machine signed it.
    Certified(DeviceCert),
    /// Nothing here may sign it: issued at the person's next sign-in when
    /// their root is in their identity record, or by pairing again.
    Pending(String),
}

/// What a re-key did.
#[derive(Debug)]
pub struct Branched {
    /// The key the machine was copied with.
    pub previous: [u8; 32],
    /// The device's own key now.
    pub device: [u8; 32],
    pub label: String,
    pub certification: Certification,
    pub stores: Vec<(String, Resealed)>,
    /// Stores that could not be re-sealed yet, with why. While any is left
    /// the journal keeps the old key and the next boot tries again.
    pub failed: Vec<(String, String)>,
}

impl Branched {
    /// Whether the re-key finished and the old key is gone.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.failed.is_empty()
    }
}

/// What [`settle`] found.
#[derive(Debug)]
pub enum Settled {
    /// No device key yet: the first boot has not made one.
    NoDevice,
    /// The platform reports no machine identity, so nothing is re-keyed.
    NoMachine,
    /// The key had no machine on record; this one is now.
    Recorded,
    /// The key belongs to this machine.
    Same,
    /// This disk was branched and the device re-keyed.
    Branched(Box<Branched>),
}

#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    machine: [u8; 16],
    from: Vec<[u8; 32]>,
    to: [u8; 32],
    label: String,
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.from.zeroize();
        self.to.zeroize();
    }
}

/// Settle which device this machine is, re-keying a branch.
///
/// `machine` is the current machine's identity ([`Machine::read`]).
///
/// # Errors
///
/// When the identity tree cannot be read or written. A re-key that stopped
/// resumes at the next call.
pub fn settle(
    paths: &IdentityPaths,
    machine: Option<&Machine>,
    branch: &Branch<'_>,
) -> Result<Settled, PersistError> {
    persist::protect_private_custody()?;
    if let Some(journal) = read_journal(paths)? {
        let Some(machine) = machine else {
            // A re-key for some machine, on one that names none: nothing
            // here can say whether it is that machine.
            return Ok(Settled::NoMachine);
        };
        let journal = if Machine(journal.machine).is(machine) {
            journal
        } else {
            // A copy of a machine that was itself re-keying: its stores are
            // under either of that machine's keys. This one gets its own.
            rebase(paths, &journal, machine)?
        };
        return resume(paths, &journal, branch).map(|done| Settled::Branched(Box::new(done)));
    }
    let Some(machine) = machine else {
        return Ok(Settled::NoMachine);
    };
    match fs::symlink_metadata(&paths.device_privkey) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Settled::NoDevice),
        Err(error) => return Err(error.into()),
    }
    match read_record(paths)? {
        None => {
            write_record(paths, machine)?;
            Ok(Settled::Recorded)
        }
        Some(recorded) if recorded.is(machine) => Ok(Settled::Same),
        Some(_) => {
            let previous = persist::read_device_key(&paths.device_privkey)?;
            let next = SigningKey::generate(&mut UnwrapErr(SysRng));
            let journal = Journal {
                version: JOURNAL_VERSION,
                machine: machine.0,
                label: branch_label(paths, &previous, &next),
                from: vec![previous.to_bytes()],
                to: next.to_bytes(),
            };
            write_journal(paths, &journal)?;
            (branch.after)(Stage::Journal)?;
            resume(paths, &journal, branch).map(|done| Settled::Branched(Box::new(done)))
        }
    }
}

fn rebase(
    paths: &IdentityPaths,
    journal: &Journal,
    machine: &Machine,
) -> Result<Journal, PersistError> {
    let mut from = journal.from.clone();
    from.push(journal.to);
    if let Ok(held) = persist::read_device_key(&paths.device_privkey) {
        from.push(held.to_bytes());
    }
    from.sort_unstable();
    from.dedup();
    if from.len() > MAX_FROM_KEYS {
        return Err(invalid("the re-key journal names too many keys"));
    }
    let next = SigningKey::generate(&mut UnwrapErr(SysRng));
    let rebased = Journal {
        version: JOURNAL_VERSION,
        machine: machine.0,
        label: relabel(&journal.label, &next),
        from,
        to: next.to_bytes(),
    };
    write_journal(paths, &rebased)?;
    Ok(rebased)
}

fn resume(
    paths: &IdentityPaths,
    journal: &Journal,
    branch: &Branch<'_>,
) -> Result<Branched, PersistError> {
    let from: Vec<SigningKey> = journal.from.iter().map(SigningKey::from_bytes).collect();
    let to = SigningKey::from_bytes(&journal.to);

    let mut stores = Vec::with_capacity(branch.stores.len());
    let mut failed = Vec::new();
    for store in &branch.stores {
        match store.reseal(&from, &to) {
            Ok(resealed) => stores.push((store.name(), resealed)),
            Err(why) => failed.push((store.name(), why)),
        }
    }
    (branch.after)(Stage::Stores)?;

    let held = persist::read_device_key(&paths.device_privkey).ok();
    if held.as_ref().map(SigningKey::to_bytes) != Some(to.to_bytes()) {
        persist::write_device_key(&paths.device_privkey, &to)?;
    }
    (branch.after)(Stage::Key)?;

    let certification = certify(paths, journal, &from, &to, branch.now_ns)?;
    (branch.after)(Stage::Certificate)?;

    if let Some(machine) = branch.machine {
        machine.renew()?;
    }
    (branch.after)(Stage::Machine)?;

    if failed.is_empty() {
        write_record(paths, &Machine(journal.machine))?;
        remove_if_present(&journal_path(paths))?;
        if let Some(parent) = paths.device_privkey.parent() {
            tilo_files::native::atomic::sync_directory(parent)?;
        }
        (branch.after)(Stage::Record)?;
    }
    Ok(Branched {
        previous: from
            .first()
            .map(|key| key.verifying_key().to_bytes())
            .unwrap_or_default(),
        device: to.verifying_key().to_bytes(),
        label: journal.label.clone(),
        certification,
        stores,
        failed,
    })
}

/// Certify `to` under the person the copied device belonged to, when this
/// machine holds that person's root; otherwise leave it pending. Every
/// certificate for a key this machine no longer holds is removed: it names
/// the source device, which is not this one.
fn certify(
    paths: &IdentityPaths,
    journal: &Journal,
    from: &[SigningKey],
    to: &SigningKey,
    now_ns: u64,
) -> Result<Certification, PersistError> {
    let device = to.verifying_key().to_bytes();
    let previous: Vec<[u8; 32]> = from
        .iter()
        .map(|key| key.verifying_key().to_bytes())
        .collect();
    let held = persist::read_device_cert(&paths.device_cert)
        .ok()
        .filter(|cert| verify_device_cert_at(cert, now_ns).is_ok());
    let certification = match held {
        Some(cert) if cert.device_public_key == device => Certification::Certified(cert),
        Some(source) if previous.contains(&source.device_public_key) => {
            self_certify(paths, &source, &journal.label, device, now_ns)?
        }
        Some(_) => Certification::Pending("the device certificate names another device".into()),
        None => Certification::Pending("the copied device had no valid certificate".into()),
    };
    remove_stale_certificates(paths, &previous)?;
    match &certification {
        Certification::Certified(_) => clear_pending_label(paths)?,
        Certification::Pending(_) => write_pending_label(paths, &journal.label)?,
    }
    Ok(certification)
}

fn self_certify(
    paths: &IdentityPaths,
    source: &DeviceCert,
    label: &str,
    device: [u8; 32],
    now_ns: u64,
) -> Result<Certification, PersistError> {
    let root = match fs::symlink_metadata(&paths.user_privkey) {
        Ok(_) => match persist::read_owned_signing_key(&paths.user_privkey) {
            Ok(root) => root,
            Err(error) => {
                return Ok(Certification::Pending(format!(
                    "the root on this machine cannot be used: {error}"
                )));
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(Certification::Pending(
                "this machine does not hold the person's root".into(),
            ));
        }
        Err(error) => return Err(error.into()),
    };
    if root.verifying_key().to_bytes() != source.user_public_key {
        return Ok(Certification::Pending(
            "the root on this machine is not the person's who owns the device".into(),
        ));
    }
    let mut cert = DeviceCert {
        user_public_key: source.user_public_key,
        device_public_key: device,
        device_label: label.to_owned(),
        capabilities: source.capabilities.clone(),
        issued_at_ns: now_ns,
        expires_at_ns: source
            .expires_at_ns
            .map(|expires| now_ns.saturating_add(expires.saturating_sub(source.issued_at_ns))),
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).map_err(|error| invalid(&error.to_string()))?;
    verify_device_cert_at(&cert, now_ns).map_err(|error| invalid(&error.to_string()))?;
    persist::write_device_cert(&paths.device_cert_for_user(&cert.user_public_key), &cert)?;
    persist::write_device_cert(&paths.device_cert, &cert)?;
    Ok(Certification::Certified(cert))
}

fn remove_stale_certificates(
    paths: &IdentityPaths,
    previous: &[[u8; 32]],
) -> Result<(), PersistError> {
    let stale = |path: &Path| {
        persist::read_device_cert(path).is_ok_and(|cert| previous.contains(&cert.device_public_key))
    };
    if stale(&paths.device_cert) {
        remove_if_present(&paths.device_cert)?;
    }
    let certs = paths.device_cert_for_user(&[0; 32]);
    let Some(directory) = certs.parent() else {
        return Ok(());
    };
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "bin") && stale(&path) {
            remove_if_present(&path)?;
        }
    }
    Ok(())
}

/// The source's name with a suffix of the new key's, so the person tells
/// the branches apart; renamed like any device later.
fn branch_label(paths: &IdentityPaths, previous: &SigningKey, next: &SigningKey) -> String {
    let source = persist::read_device_cert(&paths.device_cert)
        .ok()
        .filter(|cert| cert.device_public_key == previous.verifying_key().to_bytes())
        .and_then(|cert| device_name(&cert.device_label).map(str::to_owned));
    relabel(source.as_deref().unwrap_or("Branch"), next)
}

fn relabel(source: &str, next: &SigningKey) -> String {
    let suffix = hex::encode(&next.verifying_key().to_bytes()[..2]);
    let base = strip_branch_suffix(source.trim());
    let room = MAX_LABEL_BYTES.saturating_sub(BRANCH_WORD.len().saturating_add(suffix.len()));
    let mut end = base.len().min(room);
    while !base.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    let base = base.get(..end).unwrap_or_default().trim_end();
    if base.is_empty() || base == "Branch" {
        format!("Branch {suffix}")
    } else {
        format!("{base}{BRANCH_WORD}{suffix}")
    }
}

/// `name` without the suffix an earlier branch gave it, so a branch of a
/// branch is named after the machine the person named.
fn strip_branch_suffix(name: &str) -> &str {
    let Some((base, suffix)) = name.rsplit_once(BRANCH_WORD) else {
        return name;
    };
    if suffix.len() == 4 && suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        base
    } else {
        name
    }
}

fn valid_label(label: &str) -> bool {
    !label.is_empty() && label.len() <= MAX_LABEL_BYTES && !label.chars().any(char::is_control)
}

fn read_record(paths: &IdentityPaths) -> Result<Option<Machine>, PersistError> {
    match persist::read_regular_bounded(&machine_record(paths), 16) {
        Ok(bytes) => {
            let bytes: [u8; 16] = bytes
                .try_into()
                .map_err(|_| invalid("the machine record is malformed"))?;
            Ok(Some(Machine(bytes)))
        }
        Err(PersistError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn write_record(paths: &IdentityPaths, machine: &Machine) -> Result<(), PersistError> {
    Ok(tilo_files::native::atomic::write_with_mode(
        &machine_record(paths),
        &machine.0,
        RECORD_MODE,
    )?)
}

fn write_pending_label(paths: &IdentityPaths, label: &str) -> Result<(), PersistError> {
    let path = pending_label_path(paths);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(tilo_files::native::atomic::write_with_mode(
        &path,
        label.as_bytes(),
        persist::PUBLIC_FILE_MODE,
    )?)
}

fn read_journal(paths: &IdentityPaths) -> Result<Option<Journal>, PersistError> {
    let bytes = match persist::read_private_record(&journal_path(paths), MAX_JOURNAL_BYTES) {
        Ok(bytes) => bytes,
        Err(PersistError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let journal: Journal =
        postcard::from_bytes(&bytes).map_err(|_| invalid("the re-key journal is malformed"))?;
    if journal.version != JOURNAL_VERSION
        || journal.from.is_empty()
        || journal.from.len() > MAX_FROM_KEYS
        || journal.from.contains(&journal.to)
        || !valid_label(&journal.label)
    {
        return Err(invalid("the re-key journal is malformed"));
    }
    Ok(Some(journal))
}

fn write_journal(paths: &IdentityPaths, journal: &Journal) -> Result<(), PersistError> {
    let bytes = Zeroizing::new(
        postcard::to_allocvec(journal).map_err(|error| invalid(&error.to_string()))?,
    );
    Ok(tilo_files::native::atomic::write_with_mode(
        &journal_path(paths),
        &bytes,
        PRIVATE_MODE,
    )?)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

fn invalid(message: &str) -> PersistError {
    PersistError::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        message.to_owned(),
    ))
}

#[cfg(test)]
mod tests;
