//! Is this boot one the machine can prove it approved?
//!
//! A PIN-less keyslot releases the system volume with nothing typed, so whatever the
//! `PolicyPCR` chain accepts *is* the security boundary: the TPM will hand the key to any
//! boot whose PCR 11 the authority predicted and whose PCR 7 matches the firmware's own
//! Secure Boot state. That is only a verified boot when the boot really was verified, and
//! three conditions make it so:
//!
//! * **Secure Boot is on.** With it off, PCR 7 still holds a value and a prediction still
//!   matches, but the image that booted was never checked by anything — a changed boot
//!   could capture everything and ask the TPM itself.
//! * **`db` holds only keys this machine trusts**, and no Microsoft third-party CA. Anything
//!   else in `db` can boot under the same PCR 7, and Microsoft's third-party CA is the
//!   everyday example: it signs shims for drivers and other vendors, so leaving it in `db`
//!   means leaving in a key we did not choose.
//! * **Kernel lockdown is active.** The kernel itself refuses the paths that would let root
//!   in early userspace read or write kernel memory.
//!
//! Enrollment asks this and refuses to create the slot when a condition fails, so a machine
//! that cannot prove its boot never ends up with a volume that opens silently. Unlocking
//! asks it and only *tries* the PIN-less slot when the answer is yes, so a machine whose
//! `db` was changed after enrollment falls back to the typed secret instead of trusting a
//! boot it no longer recognises.
//!
//! # Why the fingerprint of the trust anchors is recorded
//!
//! A machine that already had the PIN-less slot when the owner key ceremony finished — an
//! OEM image, or an update — must not keep opening it against a `db` that has grown since.
//! The digest of the anchors is what "this machine trusts" means at a moment in time, so it
//! is recorded next to the rollback state at enrollment and re-checked at unlock.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Where efivarfs exposes the firmware's variables.
pub const EFIVARS: &str = "/sys/firmware/efi/efivars";

/// Where the kernel reports its lockdown state.
pub const LOCKDOWN: &str = "/sys/kernel/security/lockdown";

/// The firmware's Secure Boot database, in the global variable namespace.
const IMAGE_SECURITY_DATABASE_GUID: &str = "d719b2cb-3d3a-4596-a3bc-dad00e67656f";

/// `EFI_IMAGE_SECURITY_DATABASE`, the vendor GUID `db` lives under.
const DB_GUID: &str = IMAGE_SECURITY_DATABASE_GUID;

/// `EFI_CERT_X509_GUID`, encoded in EFI GUID byte order — an entry carrying an X.509
/// certificate.
const EFI_CERT_X509_GUID: [u8; 16] = [
    0xa1, 0x59, 0xc0, 0xa5, 0xe4, 0x94, 0xa7, 0x4a, 0x87, 0xb5, 0xab, 0x15, 0x5c, 0x2b, 0xf0, 0x72,
];

/// `EFI_CERT_SHA256_GUID` — an entry pinning one image by hash. It can admit exactly one
/// binary and cannot sign anything, so it is left out of the anchor digest: it neither
/// identifies a key this machine trusts nor widens what `db` admits on its own.
const EFI_CERT_SHA256_GUID: [u8; 16] = [
    0x26, 0x16, 0xc4, 0xc1, 0x4c, 0x50, 0x92, 0x40, 0xac, 0xa9, 0x41, 0xf9, 0x36, 0x93, 0x43, 0x28,
];

/// The certificate authorities named in a `db`, normalised so the same authority hashes the
/// same way however the firmware ordered or grouped its lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnchorDigest(pub String);

/// The firmware and kernel state one boot presents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootFacts {
    /// The `SecureBoot` variable's value, `None` when the machine has no efivarfs.
    pub secure_boot: Option<bool>,
    /// `db`'s payload with the efivarfs attribute prefix removed, `None` when unreadable.
    pub db: Option<Vec<u8>>,
    /// Kernel lockdown: either `integrity` or `confidentiality` while it is active.
    pub lockdown: Option<String>,
}

impl BootFacts {
    /// Read the running machine.
    pub fn read() -> Self {
        Self::from_efivars(Path::new(EFIVARS), Path::new(LOCKDOWN))
    }

    /// Read a machine described by `efivars` and a lockdown report, so a test can describe
    /// a boot without a firmware of its own.
    pub fn from_efivars(efivars: &Path, lockdown_report: &Path) -> Self {
        Self {
            secure_boot: read_efi_variable(efivars, "SecureBoot").map(|b| b.first() == Some(&1)),
            db: read_efi_variable(efivars, "db"),
            lockdown: std::fs::read_to_string(lockdown_report)
                .ok()
                .and_then(|text| active_lockdown(&text)),
        }
    }
}

/// `[integrity]` or `[confidentiality]` in `/sys/kernel/security/lockdown` means lockdown is
/// on; `[none]` means it is not. Anything else — an unreadable file, an unknown format — is
/// not evidence that lockdown is active, so it reports `None` rather than guessing.
pub fn active_lockdown(report: &str) -> Option<String> {
    ["integrity", "confidentiality"]
        .into_iter()
        .find(|level| report.contains(&format!("[{level}]")))
        .map(str::to_owned)
}

/// efivarfs hands back the variable's four attribute bytes, then its payload.
fn read_efi_variable(efivars: &Path, variable: &str) -> Option<Vec<u8>> {
    let path = efivars.join(format!("{variable}-{DB_GUID}"));
    let bytes = std::fs::read(&path).ok()?;
    let payload = bytes.get(4..)?;
    Some(payload.to_vec())
}

/// Why a boot is not verified. Each variant is a distinct sentence for the person deciding
/// whether to finish enrolling, so they say which condition failed rather than "not verified".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unverified {
    /// No Secure Boot state at all: not a UEFI boot, or a firmware that exposes nothing.
    NotUefi,
    /// Secure Boot is off, so nothing checked the boot image.
    SecureBootOff,
    /// `db` could not be read or could not be parsed.
    DatabaseUnreadable(String),
    /// Lockdown is not active.
    LockdownInactive,
}

impl std::fmt::Display for Unverified {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotUefi => write!(
                f,
                "this is not a UEFI Secure Boot boot, so nothing checked the boot image"
            ),
            Self::SecureBootOff => write!(
                f,
                "Secure Boot is off, so the boot image was not verified by anything"
            ),
            Self::DatabaseUnreadable(why) => {
                write!(
                    f,
                    "this machine's Secure Boot database cannot be read: {why}"
                )
            }
            Self::LockdownInactive => write!(
                f,
                "kernel lockdown is not active, so early userspace can reach kernel memory"
            ),
        }
    }
}

/// Is this boot verified — Secure Boot on, a readable `db`, and lockdown active?
///
/// Returns the anchors `db` holds, so the caller can record what this machine trusted at the
/// moment it created a PIN-less keyslot.
///
/// # Errors
///
/// Returns the [`Unverified`] condition that failed, in the order the design names them.
pub fn verified_boot(facts: &BootFacts) -> Result<AnchorDigest, Unverified> {
    match facts.secure_boot {
        None => return Err(Unverified::NotUefi),
        Some(false) => return Err(Unverified::SecureBootOff),
        Some(true) => {}
    }
    let db = facts
        .db
        .as_deref()
        .ok_or_else(|| Unverified::DatabaseUnreadable("there is no db variable".into()))?;
    let anchors = anchor_digest(db).map_err(Unverified::DatabaseUnreadable)?;
    if facts.lockdown.is_none() {
        return Err(Unverified::LockdownInactive);
    }
    Ok(anchors)
}

/// Is this boot verified, and is its `db` still the one enrollment recorded?
///
/// The second half is what keeps a recorded slot honest: a `db` that gained or lost an
/// authority after the slot was created is a different trust policy, and PCR 7 alone cannot
/// tell them apart from a legitimate firmware update.
///
/// # Errors
///
/// Returns an error naming what changed, so a boot log says why the volume asked for a
/// secret instead of opening by itself.
pub fn verified_since_enrollment(facts: &BootFacts, recorded: &AnchorDigest) -> Result<(), String> {
    let now = verified_boot(facts).map_err(|why| why.to_string())?;
    if now != *recorded {
        return Err(format!(
            "the Secure Boot database holds different authorities than the ones enrollment \
             recorded (now {}, recorded {}), so a boot matching this machine's PCR 7 need not be \
             the boot that was approved",
            now.0, recorded.0
        ));
    }
    Ok(())
}

/// The X.509 certificate authorities in an EFI signature database, as one digest over their
/// sorted DER and nothing else.
///
/// # Errors
///
/// Returns an error if the database is not a well-formed chain of EFI signature lists, or if
/// it holds no authority at all — an empty `db` trusts nothing, and a machine in that state
/// must not be treated as verified.
pub fn anchor_digest(db: &[u8]) -> Result<AnchorDigest, String> {
    let certificates = x509_entries(db)?;
    if certificates.is_empty() {
        return Err("db holds no certificate authority".into());
    }
    let mut hasher = Sha256::new();
    for der in &certificates {
        hasher.update((der.len() as u64).to_be_bytes());
        hasher.update(der);
    }
    Ok(AnchorDigest(hex(&hasher.finalize())))
}

/// Walk an EFI signature database and return the certificate payloads it carries, sorted and
/// deduplicated. Layout: `EFI_SIGNATURE_LIST` = signature-type GUID, list size, header size,
/// signature size; then entries of `owner GUID || data`.
fn x509_entries(db: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    const LIST_HEADER: usize = 16 + 4 + 4 + 4;
    const OWNER: usize = 16;
    let mut out = Vec::new();
    let mut offset = 0usize;
    while offset < db.len() {
        let header = db
            .get(offset..offset.saturating_add(LIST_HEADER))
            .ok_or("truncated EFI signature-list header")?;
        let kind: [u8; 16] = header
            .get(..16)
            .and_then(|s| s.try_into().ok())
            .ok_or("EFI signature list has no signature type")?;
        let field = |at: usize| -> Result<usize, String> {
            let end = at.checked_add(4).ok_or("EFI signature list overflows")?;
            header
                .get(at..end)
                .and_then(|s| s.try_into().ok())
                .map(u32::from_le_bytes)
                .map(|v| v as usize)
                .ok_or_else(|| "invalid EFI signature-list size field".to_string())
        };
        let list_size = field(16)?;
        let header_size = field(20)?;
        let signature_size = field(24)?;
        if list_size < LIST_HEADER || list_size < LIST_HEADER.saturating_add(header_size) {
            return Err("invalid EFI signature-list dimensions".into());
        }
        if signature_size < OWNER {
            return Err("invalid EFI signature size".into());
        }
        let end = offset.saturating_add(list_size);
        let list = db.get(offset..end).ok_or("truncated EFI signature list")?;
        let entries = list
            .get(LIST_HEADER.saturating_add(header_size)..)
            .ok_or("EFI signature entries precede their header")?;
        if kind != EFI_CERT_SHA256_GUID && !entries.len().is_multiple_of(signature_size) {
            return Err("EFI signature list has partial entries".into());
        }
        for entry in entries.chunks_exact(signature_size) {
            if kind == EFI_CERT_X509_GUID
                && let Some(der) = entry.get(OWNER..)
                && !der.is_empty()
            {
                out.push(der.to_vec());
            }
        }
        offset = end;
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The sidecar recording the anchors of the `db` enrollment saw.
///
/// Kept beside the rollback state, which is already on the encrypted root: it names no secret
/// and is only useful for deciding whether a boot is still the one that was approved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Enrollment {
    pub anchors: AnchorDigest,
    /// The certificate authorities as the person can recognise them, for a log line that
    /// says what enrolment trusted rather than a digest.
    pub authorities: Vec<String>,
}

impl Enrollment {
    /// What enrollment should record for this machine.
    ///
    /// # Errors
    ///
    /// Returns the [`Unverified`] reason the boot is not verified, so nothing is written.
    pub fn record(facts: &BootFacts) -> Result<Self, Unverified> {
        let anchors = verified_boot(facts)?;
        let authorities = facts
            .db
            .as_deref()
            .and_then(|db| x509_entries(db).ok())
            .map(|entries| {
                entries
                    .iter()
                    .map(|der| describe_certificate(der))
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            anchors,
            authorities,
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::json!({
            "anchors": self.anchors.0,
            "authorities": self.authorities,
        })
        .to_string()
    }
}

/// A certificate's subject, as far as a log line needs it. Anything unparseable is described
/// by its digest rather than dropped: an authority nobody can see is the one worth naming.
fn describe_certificate(der: &[u8]) -> String {
    tilo_crypto::inspect_certificate(der).map_or_else(
        |_| {
            // A full digest is noise in a log line; eight bytes tells two authorities
            // apart, and the module is not a place anyone matches a fingerprint by hand.
            let digest = Sha256::digest(der);
            let prefix: [u8; 8] = digest
                .get(..8)
                .and_then(|s| s.try_into().ok())
                .unwrap_or_default();
            format!("certificate {}", hex(&prefix))
        },
        |info| info.subject,
    )
}

/// Lowercase hexadecimal, the form every digest in this module is printed in.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// Refuse to enroll a PIN-less keyslot on a boot that is not verified.
///
/// # Errors
///
/// Returns an error naming the condition that failed.
pub fn require_verified(facts: &BootFacts) -> Result<Enrollment, Unverified> {
    Enrollment::record(facts)
}

/// Where the recorded enrollment lives, given the rollback state path.
pub fn enrollment_path(state_path: &Path) -> PathBuf {
    state_path.with_file_name("verified-boot.json")
}

/// Read the recorded anchor digest, if this machine has one.
pub fn load_enrollment(state_path: &Path) -> Option<AnchorDigest> {
    let text = std::fs::read_to_string(enrollment_path(state_path)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value
        .get("anchors")
        .and_then(serde_json::Value::as_str)
        .map(|a| AnchorDigest(a.to_owned()))
}

/// Record what this machine trusted when it created a PIN-less keyslot.
///
/// # Errors
///
/// Returns an error if the record cannot be written.
pub fn save_enrollment(state_path: &Path, enrollment: &Enrollment) -> Result<()> {
    let path = enrollment_path(state_path);
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &path,
        enrollment.to_json().as_bytes(),
        0o600,
    )
    .with_context(|| format!("recording the verified boot in {}", path.display()))
}

/// Deciding whether a volume that has a PIN-less keyslot may open by itself on this boot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticUnlock {
    /// Proceed: the boot is verified and the trust policy is the one enrollment recorded.
    Yes,
    /// Do not try: fall back to the typed secret, for the reason given.
    No(String),
}

impl AutomaticUnlock {
    /// # Errors
    ///
    /// Returns an error if `recorded` is `None` on a verified boot — a slot nobody recorded the
    /// trust policy for cannot be judged, so it is not opened.
    pub fn decide(facts: &BootFacts, recorded: Option<&AnchorDigest>) -> Self {
        match verified_boot(facts) {
            Err(why) => Self::No(why.to_string()),
            Ok(now) => match recorded {
                None => Self::No(
                    "this volume opens without a secret but no enrollment recorded what this \
                     machine trusted, so it is opened with the typed secret instead"
                        .into(),
                ),
                Some(at) if *at == now => Self::Yes,
                Some(at) => Self::No(format!(
                    "the Secure Boot database changed since enrollment (then {}, now {}), so \
                     this boot is not the one that was approved",
                    at.0, now.0
                )),
            },
        }
    }

    pub fn is_yes(&self) -> bool {
        matches!(self, Self::Yes)
    }

    /// # Errors
    ///
    /// Returns an error carrying the reason when the boot is not verified.
    pub fn require(&self) -> Result<()> {
        match self {
            Self::Yes => Ok(()),
            Self::No(why) => bail!("{why}"),
        }
    }
}

#[cfg(test)]
mod tests;
