//! The Linux side of the prepared-setup TPM handoff: TPM commands, the
//! privileged helper, caller-owned package custody and the local target bundle.
//!
//! Everything in the shared wire format comes from the parent module; this file
//! adds the operations that need this machine.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tilo_rollback::{
    exec::{self, Runner},
    keyslots,
    policy::{self, Prediction, Workspace},
};
use zeroize::Zeroizing;

use super::{
    MAX_SIDECAR, MAX_SIDECAR_BYTES, MAX_SIGNATURE_BYTES, MAX_UKI, PCRS, PLATFORM_PCRS,
    PREDICTIONS_DIR, RECEIVER_UID, SCHEMA, SIDECAR_FILE, SealPreparationRequest, SealStage,
    SealedPreparation, SignedTarget, TARGET_UKI_FILE, TargetAcquisition, TargetPolicyBundle, b64,
    sha256, unb64,
};

/// The release authority whose signed predictions the target already carries.
/// Kept for the installed image's own path, but the privileged helper uses the
/// authority pinned into its binary instead of any caller-supplied file.
pub const PUBLIC_KEY_FILE: &str = "/usr/share/tilo/tpm2-pcr-signing.public.pem";
/// The release authority embedded in the shipped helper. This is the anchor
/// for every target policy review: a caller cannot substitute a key file.
pub const PINNED_RELEASE_AUTHORITY_PEM: &str =
    include_str!("../../../../../../keys/tpm2-pcr-signing.public.pem");
/// Where the live medium publishes the exact target boot bundle.
pub const LIVE_TARGET_ROOTS: [&str; 3] = [
    "/boot/tilo-target",
    "/efi/tilo-target",
    "/boot/efi/tilo-target",
];
/// The live image's own generation file. It selects among retained signed
/// predictions; a downloaded bundle carries exactly one generation instead.
const GENERATION_FILE: &str = "/usr/lib/tilo/generation";

pub(crate) const MAX_SEAL_INPUT: usize = 128 * 1024;
/// The canonical prepared package is at most 32 MiB + 16 KiB. The helper
/// reads at most this much so an unprivileged caller cannot use it as a root
/// file oracle for an arbitrary large file.
const MAX_PACKAGE: u64 = 32 * 1024 * 1024 + 16 * 1024;
const MAX_PREDICTION: u64 = 16 * 1024;
const MAX_SIGNATURE: u64 = 4 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn refused(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message.into())
}

fn other(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn cancelled() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "the preparation was cancelled")
}

fn check_cancel(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(cancelled());
    }
    Ok(())
}

// --- target policy review ---------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PredictionDocument {
    generation: u64,
    pcr_bank: String,
    pcrs: Vec<u32>,
    approved_policy: String,
    signature: String,
    #[serde(default)]
    label: String,
}

/// Re-verify a target policy against the trusted release authority, not against
/// a caller-supplied anchor. A helper that accepts a serialized `SignedTarget`
/// from stdin must call this before sealing: otherwise an unprivileged caller
/// could keep the real authority digest while choosing another
/// approved-policy/signature pair.
///
/// # Errors
///
/// The target policy is malformed or the authority does not verify it.
pub fn validate_signed(target: &SignedTarget, public_key_pem: &str) -> io::Result<()> {
    target.validate()?;
    let document = serde_json::json!({
        "generation": target.generation,
        "pcr_bank": target.pcr_bank,
        "pcrs": target.pcrs,
        "approved_policy": target.approved_policy,
        "signature": target.signature,
        "label": target.label,
    });
    let bytes = serde_json::to_vec(&document).map_err(io::Error::other)?;
    tilo_crypto::verify_pcr_prediction(public_key_pem, &bytes, target.generation).map_err(invalid)
}

/// Verify one authority-signed sidecar against the exact target UKI and the
/// release authority. Nothing here contacts or trusts the TPM: this proves what
/// the future boot will measure, which is what sealing binds.
///
/// # Errors
///
/// The prediction or target signature does not verify against the authority, or
/// the generation and policy shape are unsupported.
pub fn review(
    prediction_json: &[u8],
    public_key_pem: &[u8],
    target_uki: &[u8],
    target_uki_signature: &[u8],
    generation: u64,
) -> io::Result<SignedTarget> {
    let public_key = std::str::from_utf8(public_key_pem)
        .map_err(|_| invalid("release authority key is not UTF-8 PEM"))?;
    tilo_crypto::verify_pcr_prediction(public_key, prediction_json, generation).map_err(invalid)?;
    let document: PredictionDocument = serde_json::from_slice(prediction_json)
        .map_err(|_| invalid("signed target prediction is malformed"))?;
    if document.generation != generation
        || document.pcr_bank != "sha256"
        || document.pcrs.as_slice() != PCRS
    {
        return Err(invalid(
            "signed target prediction is not the exact generation's SHA-256 PCR 11 policy",
        ));
    }
    let approved = unb64(&document.approved_policy, "approved policy", 64)?;
    if approved.len() != 32 {
        return Err(invalid("approved policy is not a SHA-256 digest"));
    }
    let signature = unb64(
        &document.signature,
        "prediction signature",
        MAX_SIGNATURE_BYTES,
    )?;
    if !(256..=1024).contains(&signature.len()) {
        return Err(invalid("prediction signature has an invalid size"));
    }
    if target_uki.is_empty() || target_uki.len() as u64 > MAX_UKI {
        return Err(invalid("target UKI is empty or oversized"));
    }
    tilo_crypto::verify_rsa_sha256(public_key, target_uki, target_uki_signature)
        .map_err(|_| invalid("target UKI does not carry the release authority's signature"))?;
    Ok(SignedTarget {
        schema: SCHEMA,
        generation,
        pcr_bank: document.pcr_bank,
        pcrs: document.pcrs,
        platform_pcrs: PLATFORM_PCRS.to_vec(),
        approved_policy: document.approved_policy,
        signature: document.signature,
        target_uki_sha256: sha256(target_uki),
        authority_sha256: sha256(public_key_pem),
        label: document.label,
    })
}

/// The signed prediction these policy fields carry, decoded for the TPM policy
/// builder.
///
/// # Errors
///
/// The approved policy or signature is not standard base64 within its bound.
pub fn prediction(target: &SignedTarget) -> io::Result<Prediction> {
    Ok(Prediction {
        approved_policy: unb64(&target.approved_policy, "approved policy", 64)?,
        signature: unb64(
            &target.signature,
            "prediction signature",
            MAX_SIGNATURE_BYTES,
        )?,
        label: target.label.clone(),
    })
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(invalid(
            "signed target artifact is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1)).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(invalid("signed target artifact exceeded its bound"));
    }
    Ok(bytes)
}

/// Review the exact signed target artifacts under `root`.
///
/// # Errors
///
/// A bundle file is missing, changes while it is read, or fails authority
/// verification.
pub fn review_paths(root: &Path, public_key: &Path, generation: u64) -> io::Result<SignedTarget> {
    let prediction_path = root.join(format!("{PREDICTIONS_DIR}/generation-{generation}.json"));
    let signature = root.join(format!(
        "{PREDICTIONS_DIR}/generation-{generation}.json.uki.sig"
    ));
    review(
        &read_bounded(&prediction_path, MAX_PREDICTION)?,
        &read_bounded(public_key, MAX_PREDICTION)?,
        &read_bounded(&root.join(TARGET_UKI_FILE), MAX_UKI)?,
        &read_bounded(&signature, MAX_SIGNATURE)?,
        generation,
    )
}

/// Review a target bundle directory by an explicit authority. A bundle is the
/// same shape the live medium carries: `tilo-target.efi` and exactly one signed
/// `predictions/generation-<N>.json` with its `.uki.sig`.
///
/// # Errors
///
/// The directory or prediction files are missing, malformed, or fail authority
/// verification.
pub fn review_bundle(root: &Path, authority_pem: &[u8]) -> io::Result<SignedTarget> {
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    if !directory.metadata()?.is_dir() {
        return Err(invalid("target bundle is not a directory"));
    }
    review_open_bundle(&directory, authority_pem, None, None)
}

/// Review a live medium's target bundle. Retained signed predictions may name
/// several generations; the image's generation file selects the one the
/// running boot will actually measure.
///
/// # Errors
///
/// The directory is missing, or the selected prediction and target fail
/// authority, signature, or generation verification.
pub fn review_live_bundle(root: &Path, authority_pem: &[u8]) -> io::Result<SignedTarget> {
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root)?;
    if !directory.metadata()?.is_dir() {
        return Err(invalid("target bundle is not a directory"));
    }
    review_open_bundle(&directory, authority_pem, None, live_generation())
}

/// Review a bundle acquired by the release download path. The pinned release
/// authority, never a caller-selected key file, decides whether the target and
/// its generation are authentic.
///
/// # Errors
///
/// The acquired bundle is missing, malformed, or not signed by the pinned
/// release authority.
pub fn review_acquired_bundle(root: &Path) -> io::Result<SignedTarget> {
    review_bundle(root, PINNED_RELEASE_AUTHORITY_PEM.as_bytes())
}

/// Review a caller-owned downloaded bundle through held, no-follow descriptors.
/// The helper uses this so the caller cannot swap a file between review and
/// sealing.
///
/// # Errors
///
/// The directory is not the original caller's private no-follow directory, or
/// the bundle fails authority, signature or generation verification.
pub fn review_held_bundle(
    root: &Path,
    caller_uid: u32,
    authority_pem: &[u8],
) -> io::Result<SignedTarget> {
    let directory = open_caller_directory(root, caller_uid)?;
    review_open_bundle(&directory, authority_pem, Some(caller_uid), None)
}

fn review_open_bundle(
    root: &File,
    authority_pem: &[u8],
    caller_uid: Option<u32>,
    generation: Option<u64>,
) -> io::Result<SignedTarget> {
    let predictions = cap_primitives::fs::open_dir_nofollow(root, Path::new(PREDICTIONS_DIR))
        .map_err(|_| invalid("target bundle has no predictions directory"))?;
    let metadata = predictions.metadata()?;
    if let Some(caller_uid) = caller_uid
        && (metadata.uid() != caller_uid || metadata.mode() & 0o077 != 0)
    {
        return Err(refused(
            "the target bundle predictions directory is not private to the original caller",
        ));
    }
    let mut names = Vec::new();
    for entry in cap_std::fs::Dir::from_std_file(predictions.try_clone()?).entries()? {
        let name = entry?.file_name();
        if let Some(text) = name.to_str()
            && let Some(generation) = prediction_generation(text)
        {
            names.push((generation, text.to_owned()));
        }
    }
    let (generation, name) = if let Some(expected) = generation {
        names
            .iter()
            .find(|(generation, _)| *generation == expected)
            .ok_or_else(|| {
                invalid(format!(
                    "target bundle carries no signed prediction for generation {expected}"
                ))
            })?
    } else {
        match names.as_slice() {
            [] => {
                return Err(invalid(
                    "target bundle carries no signed generation prediction",
                ));
            }
            [only] => only,
            _ => {
                return Err(invalid(
                    "target bundle carries more than one signed generation prediction",
                ));
            }
        }
    };
    let prediction = read_held_file(&predictions, OsStr::new(name), caller_uid, MAX_PREDICTION)?;
    let signature_name = format!("{name}.uki.sig");
    let signature = read_held_file(
        &predictions,
        OsStr::new(&signature_name),
        caller_uid,
        MAX_SIGNATURE,
    )?;
    let target = read_held_file(root, OsStr::new(TARGET_UKI_FILE), caller_uid, MAX_UKI)?;
    review(&prediction, authority_pem, &target, &signature, *generation)
}

/// Review the live medium's target boot bundle. `Ok(None)` is the honest
/// no-useful-policy answer: the caller must use the deferred journal instead of
/// inventing a TPM-only blob.
///
/// # Errors
///
/// More than one live bundle exists, or the single bundle fails verification.
pub fn review_live_target() -> io::Result<Option<SignedTarget>> {
    review_live_target_bundle().map(|bundle| bundle.map(|(target, _)| target))
}

/// The live target policy and the directory it was read from, so a privileged
/// helper can re-review the same bundle instead of trusting a digest sent by
/// the caller.
///
/// # Errors
///
/// More than one live bundle exists, or the single bundle fails verification.
pub fn review_live_target_bundle() -> io::Result<Option<(SignedTarget, PathBuf)>> {
    let mut found = Vec::new();
    for root in LIVE_TARGET_ROOTS {
        let root = PathBuf::from(root);
        if root.join(TARGET_UKI_FILE).is_file() {
            found.push(root);
        }
    }
    match found.as_slice() {
        [] => Ok(None),
        [root] => review_live_bundle(root, PINNED_RELEASE_AUTHORITY_PEM.as_bytes())
            .map(|target| Some((target, root.clone()))),
        _ => Err(invalid(
            "the live medium carries more than one target boot bundle",
        )),
    }
}

/// Parse `generation-<N>.json` into its dense generation number. The signed
/// prediction document must repeat the same number; this is only the filename
/// that selects the sidecar inside a bundle.
fn live_generation() -> Option<u64> {
    std::fs::read_to_string(GENERATION_FILE)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn prediction_generation(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("generation-")?.strip_suffix(".json")?;
    if digits.is_empty()
        || digits.len() > 20
        || digits.starts_with('0')
        || !digits.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    digits.parse().ok()
}

// --- caller-owned custody ---------------------------------------------------

/// Open a directory the original caller owns and nobody else can enter.
fn open_caller_directory(path: &Path, caller_uid: u32) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(refused(
            "the prepared package and target bundle must be absolute caller-owned paths",
        ));
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.uid() != caller_uid || metadata.mode() & 0o077 != 0 {
        return Err(refused(
            "the prepared package or target bundle directory is not private to the original caller",
        ));
    }
    Ok(directory)
}

/// Read one bounded, no-follow regular file from a directory this process
/// already holds. With `caller_uid`, the file must stay the original caller's
/// private single-link file for the whole read.
fn read_held_file(
    parent: &File,
    name: &OsStr,
    caller_uid: Option<u32>,
    limit: u64,
) -> io::Result<Vec<u8>> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions as CapOptions};

    let mut options = CapOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = cap_primitives::fs::open(parent, Path::new(name), &options)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > limit {
        return Err(invalid(
            "signed target artifact is not a bounded regular file",
        ));
    }
    if let Some(caller_uid) = caller_uid
        && (before.uid() != caller_uid || before.mode() & 0o077 != 0 || before.nlink() != 1)
    {
        return Err(refused(
            "the prepared package or target bundle is not the exact private caller-owned file",
        ));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(refused(
            "the prepared package or target bundle changed while it was read",
        ));
    }
    Ok(bytes)
}

/// Read a caller-owned prepared package through an owned, no-follow parent
/// directory. The path is never adopted as authority: its parent and file must
/// both belong to the original caller, stay private, and keep one link while
/// the helper reads and hashes the exact bytes.
fn read_caller_package(
    path: &Path,
    caller_uid: u32,
    limit: u64,
) -> io::Result<(Vec<u8>, [u8; 32])> {
    let parent_path = path
        .parent()
        .ok_or_else(|| refused("the prepared package has no parent directory"))?;
    let name = path
        .file_name()
        .ok_or_else(|| refused("the prepared package has no filename"))?;
    let parent = open_caller_directory(parent_path, caller_uid)?;
    let bytes = read_held_file(&parent, name, Some(caller_uid), limit)?;
    let digest = sha256(&bytes);
    Ok((bytes, digest))
}

/// Write the sidecar beside a package in a private directory.
///
/// # Errors
///
/// The sidecar is invalid or its private file cannot be written.
pub fn write_sidecar(directory: &Path, sealed: &SealedPreparation) -> io::Result<PathBuf> {
    let path = directory.join(SIDECAR_FILE);
    tilo_files::native::atomic::write_with_mode(&path, &sealed.encode()?, 0o600)?;
    Ok(path)
}

/// Read a sidecar from a private directory without following links.
///
/// # Errors
///
/// The sidecar path is not private or its bytes are invalid.
pub fn read_sidecar(directory: &Path) -> io::Result<Option<SealedPreparation>> {
    let path = directory.join(SIDECAR_FILE);
    private_sidecar(&path)
}

/// Read a sidecar from an explicit path without following links.
///
/// # Errors
///
/// The sidecar path is not private or its bytes are invalid.
pub fn read_sidecar_file(path: &Path) -> io::Result<Option<SealedPreparation>> {
    private_sidecar(path)
}

fn private_sidecar(path: &Path) -> io::Result<Option<SealedPreparation>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
        return Err(refused(
            "prepared TPM handoff is not a private regular file",
        ));
    }
    SealedPreparation::decode(&read_bounded(path, MAX_SIDECAR)?).map(Some)
}

// --- sealing and unsealing --------------------------------------------------

/// The unprivileged process that invoked sudo owns the package and directory.
/// `sudo` is the only supported production caller; a root process has no
/// original unprivileged UID to bind, so it must use the explicit test path.
fn original_caller_uid() -> io::Result<u32> {
    let value = std::env::var("SUDO_UID")
        .map_err(|_| refused("the privileged helper requires the original caller's UID"))?;
    let uid = value
        .parse::<u32>()
        .map_err(|_| refused("the original caller's UID is malformed"))?;
    if uid == 0 {
        return Err(refused("the original caller must be an unprivileged user"));
    }
    Ok(uid)
}

/// Inputs to sealing. The package digest binds the sidecar to the exact
/// canonical ciphertext that is installed.
pub struct SealRequest<'a> {
    pub target: &'a SignedTarget,
    pub source_public_key: [u8; 32],
    pub package_sha256: [u8; 32],
    pub receiver_uid: u32,
    pub secret: &'a [u8],
}

/// Read the endorsement key's TPM Name. `Name = nameAlg || H(public area)` is
/// the stable identity of this TPM's EK; a sidecar copied elsewhere names a
/// different receiver before any policy session is attempted.
fn receiver_name(runner: &dyn Runner, ws: &Workspace) -> io::Result<Vec<u8>> {
    policy::flush(runner);
    let context = ws.path("ek.ctx");
    let public = ws.path("ek.pub");
    exec::require(
        runner,
        "tpm2_createek",
        &[
            OsStr::new("-G"),
            OsStr::new("rsa"),
            OsStr::new("-c"),
            context.as_os_str(),
            OsStr::new("-u"),
            public.as_os_str(),
        ],
        "creating this TPM's endorsement key",
    )
    .map_err(io::Error::other)?;
    let name = ws.path("ek.name");
    exec::require(
        runner,
        "tpm2_readpublic",
        &[
            OsStr::new("-c"),
            context.as_os_str(),
            OsStr::new("-n"),
            name.as_os_str(),
        ],
        "reading this TPM's endorsement-key name",
    )
    .map_err(io::Error::other)?;
    let bytes = std::fs::read(&name)?;
    policy::flush(runner);
    if bytes.is_empty() || bytes.len() > 128 {
        return Err(invalid("endorsement-key name has an invalid length"));
    }
    Ok(bytes)
}

/// Seal the machine-generated package secret to the verified target policy and
/// this actual TPM. No PIN and no login password participate.
///
/// # Errors
///
/// The target policy, secret, workspace, or TPM operation is invalid.
pub fn seal(
    runner: &dyn Runner,
    ws: &Workspace,
    request: &SealRequest<'_>,
    authority_pem: &[u8],
) -> io::Result<SealedPreparation> {
    request.target.validate()?;
    if request.secret.len() < 15 {
        return Err(invalid(
            "preparation secret is too short for the canonical package carrier",
        ));
    }
    if request.source_public_key == [0; 32]
        || request.package_sha256 == [0; 32]
        || request.receiver_uid != RECEIVER_UID
    {
        return Err(invalid(
            "preparation identity, package digest or receiving account is missing",
        ));
    }
    if sha256(authority_pem) != request.target.authority_sha256 {
        return Err(invalid(
            "target boot policy was verified by a different release authority",
        ));
    }
    let public_key_pem = std::str::from_utf8(authority_pem)
        .map_err(|_| invalid("release authority key is not UTF-8 PEM"))?;
    validate_signed(request.target, public_key_pem)?;
    let authority_path = ws.path("release-authority.pem");
    std::fs::write(&authority_path, authority_pem)?;
    let key_name = policy::key_name(runner, ws, &authority_path).map_err(io::Error::other)?;
    let boot = policy::BootPolicy {
        generation: request.target.generation,
        key_name: &key_name,
        pcr_bank: &request.target.pcr_bank,
        pcrs: &request.target.pcrs,
        platform_pcrs: &request.target.platform_pcrs,
        pin: false,
    };
    let digest = policy::seal_boot_policy(runner, ws, &boot).map_err(io::Error::other)?;
    let (public, private) =
        keyslots::seal(runner, ws, request.secret, &digest, None).map_err(io::Error::other)?;
    let receiver = receiver_name(runner, ws)?;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(io::Error::other)?;
    let mut sealed = SealedPreparation {
        schema: SCHEMA,
        policy: request.target.clone(),
        source_public_key: request.source_public_key,
        package_sha256: request.package_sha256,
        receiver_uid: request.receiver_uid,
        receiver_tpm_name: b64(&receiver),
        key_name: b64(&key_name),
        sealed_public: b64(&public),
        sealed_private: b64(&private),
        policy_digest: b64(&digest),
        seal_nonce: b64(&nonce),
        binding_sha256: String::new(),
    };
    sealed.seal_binding()?;
    sealed.validate()?;
    Ok(sealed)
}

/// Open a sidecar on the real installed boot. `Ok(None)` means the signed
/// prediction does not describe this live state; `Err` means the sidecar names
/// another TPM, another identity/package, or the TPM mechanism failed.
///
/// # Errors
///
/// The sidecar, runtime generation, receiver TPM, or TPM policy is invalid.
pub fn unseal(
    runner: &dyn Runner,
    ws: &Workspace,
    sidecar: &SealedPreparation,
    authority_pem: &[u8],
    generation: u64,
) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    sidecar.validate()?;
    if sidecar.policy.generation != generation {
        return Err(invalid(format!(
            "prepared handoff is for generation {}, not the running generation {generation}",
            sidecar.policy.generation
        )));
    }
    if sha256(authority_pem) != sidecar.policy.authority_sha256 {
        return Err(invalid(
            "prepared handoff was signed by a different release authority",
        ));
    }
    let public_key_pem = std::str::from_utf8(authority_pem)
        .map_err(|_| invalid("release authority key is not UTF-8 PEM"))?;
    validate_signed(&sidecar.policy, public_key_pem)?;
    let public_key_path = ws.path("release-authority.pem");
    std::fs::write(&public_key_path, authority_pem)?;
    let expected_receiver = unb64(&sidecar.receiver_tpm_name, "receiver TPM name", 128)?;
    let actual_receiver = receiver_name(runner, ws)?;
    if actual_receiver != expected_receiver {
        return Err(refused(
            "prepared handoff was sealed for another TPM; this machine cannot open it",
        ));
    }
    let key_name = unb64(&sidecar.key_name, "prediction authority name", 128)?;
    let boot = policy::BootPolicy {
        generation: sidecar.policy.generation,
        key_name: &key_name,
        pcr_bank: &sidecar.policy.pcr_bank,
        pcrs: &sidecar.policy.pcrs,
        platform_pcrs: &sidecar.policy.platform_pcrs,
        pin: false,
    };
    let sealed_public = unb64(&sidecar.sealed_public, "sealed public area", 4096)?;
    let sealed_private = unb64(&sidecar.sealed_private, "sealed private area", 4096)?;
    let policy_digest = unb64(&sidecar.policy_digest, "sealed policy digest", 64)?;
    let artifact = keyslots::SealedArtifact {
        public: &sealed_public,
        private: &sealed_private,
        policy_digest: &policy_digest,
    };
    let predictions = [prediction(&sidecar.policy)?];
    keyslots::unseal_boot(runner, ws, &artifact, &boot, &predictions, &public_key_path)
        .map_err(io::Error::other)
}

/// Seal from the vetted privileged helper's stdin. The input names the exact
/// caller-owned package, the acquired target bundle and the reviewed
/// generation/UKI digest, never a serialized boot policy or a public-key path.
/// The helper re-reviews the bundle against the authority pinned into its
/// binary, so an unprivileged caller cannot choose the trust anchor.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SealInput {
    generation: u64,
    target_uki_sha256: [u8; 32],
    target_bundle_path: String,
    package_path: String,
    package_sha256: [u8; 32],
    source_public_key: [u8; 32],
    secret: String,
}

/// Read the helper request through a bound. The unprivileged caller controls
/// this stream, so the root process never grows its heap to the caller's pipe.
///
/// # Errors
///
/// The request is oversized or malformed, or the package and target review fails.
pub fn seal_from_reader(mut input: impl Read, output: &mut impl Write) -> io::Result<()> {
    let mut bytes = Vec::new();
    input
        .by_ref()
        .take(
            u64::try_from(MAX_SEAL_INPUT)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_SEAL_INPUT {
        return Err(invalid("prepared TPM seal input exceeds its bound"));
    }
    seal_from_json(&bytes, output)
}

///
/// # Errors
///
/// The request is malformed, or the package and target review fails.
pub fn seal_from_json(input: &[u8], output: &mut impl Write) -> io::Result<()> {
    let caller_uid = original_caller_uid()?;
    let work = tempfile::tempdir()?;
    let runner = tilo_rollback::exec::Real::new();
    let workspace = tilo_rollback::policy::Workspace::new(work.path());
    seal_from_json_with(
        input,
        output,
        PINNED_RELEASE_AUTHORITY_PEM.as_bytes(),
        caller_uid,
        &runner,
        &workspace,
    )
}

/// Review the target bundle the request names. The live medium's root-owned
/// bundle is a fixed image path; every other bundle must be the original
/// caller's private no-follow directory. Both are verified by the pinned
/// authority, never by a caller-supplied key.
fn review_request_target(
    path: &Path,
    caller_uid: u32,
    authority_pem: &[u8],
) -> io::Result<SignedTarget> {
    if LIVE_TARGET_ROOTS
        .iter()
        .any(|root| path == Path::new(*root))
    {
        review_bundle(path, authority_pem)
    } else {
        review_held_bundle(path, caller_uid, authority_pem)
    }
}

/// Variant with an explicit runner, workspace and authority, so the real TPM
/// simulator can drive this exact helper path without a process-wide TCTI
/// override. Production always obtains the authority from
/// [`PINNED_RELEASE_AUTHORITY_PEM`] and `caller_uid` from `SUDO_UID`.
///
/// # Errors
///
/// The request is malformed, or the package and target review fails.
pub fn seal_from_json_with(
    input: &[u8],
    output: &mut impl Write,
    authority_pem: &[u8],
    caller_uid: u32,
    runner: &dyn Runner,
    workspace: &Workspace,
) -> io::Result<()> {
    if input.len() > MAX_SEAL_INPUT {
        return Err(invalid("prepared TPM seal input exceeds its bound"));
    }
    let input: SealInput = serde_json::from_slice(input)
        .map_err(|_| invalid("prepared TPM seal input is malformed"))?;
    let secret = Zeroizing::new(input.secret);
    if secret.len() < 15 {
        return Err(invalid(
            "preparation secret is too short for the canonical package carrier",
        ));
    }
    if input.source_public_key == [0; 32] || input.package_sha256 == [0; 32] {
        return Err(invalid("preparation identity or package digest is missing"));
    }
    let target = review_request_target(
        Path::new(&input.target_bundle_path),
        caller_uid,
        authority_pem,
    )?;
    if target.generation() != input.generation
        || target.target_uki_sha256() != input.target_uki_sha256
    {
        return Err(invalid(
            "the request does not name the exact signed target boot policy",
        ));
    }
    let (_package, digest) =
        read_caller_package(Path::new(&input.package_path), caller_uid, MAX_PACKAGE)?;
    if digest != input.package_sha256 {
        return Err(refused(
            "the prepared package changed after the caller hashed it",
        ));
    }
    let sealed = seal(
        runner,
        workspace,
        &SealRequest {
            target: &target,
            source_public_key: input.source_public_key,
            package_sha256: digest,
            receiver_uid: RECEIVER_UID,
            secret: secret.as_bytes(),
        },
        authority_pem,
    )?;
    output.write_all(&sealed.encode()?)?;
    output.flush()
}

/// Run the vetted privileged helper for one prepared package and write the
/// returned public sidecar beside it.
///
/// The unprivileged installer cannot open the TPM. The helper runs under its
/// own narrow sudo rule, re-reviews the same signed bundle against its pinned
/// authority, receives the secret on stdin (never argv), and returns only the
/// public sidecar.
///
/// # Errors
///
/// The request is cancelled, the package cannot be read, the helper refuses or
/// answers with another binding, or the sidecar cannot be written.
pub fn seal_prepared_handoff(
    request: &SealPreparationRequest<'_>,
) -> io::Result<SealedPreparation> {
    check_cancel(request.cancel)?;
    (request.progress)(SealStage::ReadingPackage)?;
    let package_bytes = read_bounded(request.package, MAX_PACKAGE)?;
    let package_sha256 = sha256(&package_bytes);
    let package_path = request
        .package
        .to_str()
        .ok_or_else(|| other("The prepared package path is not UTF-8"))?;
    let target_bundle_path = request
        .target
        .bundle()
        .to_str()
        .ok_or_else(|| other("The target bundle path is not UTF-8"))?;
    let input = serde_json::to_vec(&serde_json::json!({
        "generation": request.target.generation(),
        "target_uki_sha256": request.target.target_uki_sha256(),
        "target_bundle_path": target_bundle_path,
        "package_path": package_path,
        "package_sha256": package_sha256,
        "source_public_key": request.source_public_key,
        "secret": request.secret,
    }))?;
    check_cancel(request.cancel)?;
    (request.progress)(SealStage::Sealing)?;
    let mut command = std::process::Command::new("/usr/bin/sudo");
    command.args([
        "-n",
        "/usr/libexec/tilo-prepared-setup",
        "--seal-prepared-handoff",
    ]);
    let output = bounded_command::run_prepared(
        command,
        Some(&input),
        MAX_SIDECAR_BYTES,
        Duration::from_secs(120),
    )
    .map_err(io::Error::other)?;
    if !output.status.success() || output.stdout.len() > MAX_SIDECAR_BYTES {
        return Err(other(
            "The privileged TPM handoff helper refused the signed target",
        ));
    }
    let sealed = SealedPreparation::decode(&output.stdout)?;
    if sealed.source_public_key() != request.source_public_key
        || sealed.package_sha256() != package_sha256
        || sealed.generation() != request.target.generation()
        || sealed.receiver_uid() != RECEIVER_UID
    {
        return Err(other(
            "The privileged TPM handoff helper returned another binding",
        ));
    }
    check_cancel(request.cancel)?;
    (request.progress)(SealStage::WritingSidecar)?;
    write_sidecar(request.sidecar_directory, &sealed)?;
    Ok(sealed)
}

/// Verify a sidecar and the exact package it names before first boot. This is
/// the same canonical parser the sealing side uses, so a sidecar either passes
/// both or neither. The check is structural and binds the package digest; the
/// signed target policy is authenticated only by [`unseal`] against its pinned
/// release authority before any secret is released.
///
/// # Errors
///
/// The sidecar or package is not a bounded no-follow regular file reached
/// through a plain path, it changes while it is read, the sidecar is malformed
/// or edited, or it names another package digest.
pub fn preflight_handoff(sidecar_path: &Path, package_path: &Path) -> io::Result<()> {
    tilo_handoff::prepared::preflight_handoff(sidecar_path, package_path)
}

/// Read and structurally verify the sidecar and its exact package, returning
/// the decoded sidecar that was checked. The privileged first-boot unsealer
/// uses this so the policy it authenticates is the one this preflight read.
///
/// # Errors
///
/// The sidecar or package is not a bounded no-follow regular file reached
/// through a plain path, it changes while it is read, the sidecar is malformed
/// or edited, or it names another package digest.
pub fn read_prepared_handoff(
    sidecar_path: &Path,
    package_path: &Path,
) -> io::Result<SealedPreparation> {
    tilo_handoff::prepared::read_prepared_handoff(sidecar_path, package_path)
}

// --- acquiring the target policy --------------------------------------------

/// Resolve the target boot policy for this same-computer preparation: the live
/// medium's own signed bundle when this process is that image, otherwise the
/// small bundle published by the release catalog. A host that cannot prove a
/// signed target keeps the deferred journal instead of claiming a TPM binding.
///
/// # Errors
///
/// The policy cannot be downloaded, or the acquired bundle does not match it.
pub fn target_for_preparation(
    request: &super::AcquireRequest<'_>,
) -> io::Result<Option<TargetAcquisition>> {
    if let Ok(Some((target, bundle))) = review_live_target_bundle() {
        return Ok(Some(TargetAcquisition::new(target, bundle)));
    }
    let Some(policy) = request.policy else {
        return Ok(None);
    };
    match acquire_target_policy(policy, request.cancel) {
        Ok(acquisition) => Ok(Some(acquisition)),
        // No verified target is the honest deferred fallback: the retained
        // journal and public install profile still carry the reviewed choices,
        // and first boot asks for the canonical approval instead of claiming a
        // TPM binding this host cannot prove.
        Err(error) if error.kind() == io::ErrorKind::Interrupted => Err(error),
        Err(_) => Ok(None),
    }
}

/// Download, verify and cache the target policy bundle named by the release
/// catalog. The archive is bounded and digest-checked before any entry is
/// written; only the target UKI and its signed prediction sidecars are
/// accepted.
///
/// # Errors
///
/// The request is cancelled, the download fails or changes origin, the archive
/// is oversized or carries an unexpected entry, or the target differs from the
/// catalog's digest.
pub fn acquire_target_policy(
    policy: &TargetPolicyBundle,
    cancel: &AtomicBool,
) -> io::Result<TargetAcquisition> {
    check_cancel(cancel)?;
    let parent = cache_root().join("target-policy");
    std::fs::create_dir_all(&parent)
        .map_err(|error| other(format!("Could not create the target policy cache: {error}")))?;
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| other(format!("Could not secure the target policy cache: {error}")))?;
    let final_root = parent.join(&policy.archive_sha256);
    if final_root.join(TARGET_UKI_FILE).is_file() {
        secure_target_policy_bundle(&final_root)?;
        return verify_target_policy_bundle(&final_root, policy);
    }
    let part = download_target_policy(policy, &parent, cancel)?;
    let extracted = extract_target_policy(&part, &parent, &final_root, policy, cancel);
    let _ = std::fs::remove_file(&part);
    extracted?;
    secure_target_policy_bundle(&final_root)?;
    verify_target_policy_bundle(&final_root, policy)
}

fn download_target_policy(
    policy: &TargetPolicyBundle,
    parent: &Path,
    cancel: &AtomicBool,
) -> io::Result<PathBuf> {
    let part = parent.join(format!(".{}.part", policy.archive_sha256));
    let _ = std::fs::remove_file(&part);
    let trusted = reqwest::Url::parse(&policy.url)
        .map_err(|_| invalid("The target policy URL is invalid."))?;
    if trusted.scheme() != "https"
        || !trusted.username().is_empty()
        || trusted.password().is_some()
        || trusted.fragment().is_some()
    {
        return Err(invalid("The target policy URL is not a trusted download."));
    }
    let client = tilo_net::http::blocking_client().build().map_err(|error| {
        other(format!(
            "Could not start the target policy download: {error}"
        ))
    })?;
    let response = client
        .get(trusted.clone())
        .send()
        .map_err(|error| other(format!("Could not download the target policy: {error}")))?;
    if !response.status().is_success() {
        return Err(other(format!(
            "The target policy download returned {}.",
            response.status()
        )));
    }
    let actual = response.url().clone();
    if !same_origin(&actual, &trusted) {
        return Err(invalid(
            "The target policy download left its trusted origin.",
        ));
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|error| {
            other(format!(
                "Could not create the target policy download: {error}"
            ))
        })?;
    let mut response = response.take(policy.archive_size.saturating_add(1));
    let mut received = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    loop {
        check_cancel(cancel)?;
        let read = response.read(&mut buffer).map_err(|error| {
            other(format!(
                "The target policy download was interrupted: {error}"
            ))
        })?;
        if read == 0 {
            break;
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| invalid("Could not bound the target policy response."))?;
        file.write_all(chunk)
            .map_err(|error| other(format!("Could not save the target policy: {error}")))?;
        received = received.saturating_add(read as u64);
        if received > policy.archive_size {
            let _ = std::fs::remove_file(&part);
            return Err(invalid(
                "The target policy download exceeded its declared size.",
            ));
        }
    }
    file.flush()
        .map_err(|error| other(format!("Could not save the target policy: {error}")))?;
    drop(file);
    let digest = sha256_file(&part)?;
    if received != policy.archive_size || digest != policy.archive_sha256 {
        let _ = std::fs::remove_file(&part);
        return Err(invalid(
            "The target policy download does not match the release catalog.",
        ));
    }
    Ok(part)
}

fn extract_target_policy(
    part: &Path,
    parent: &Path,
    final_root: &Path,
    policy: &TargetPolicyBundle,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let staging = tempfile::Builder::new()
        .prefix("target-policy-")
        .tempdir_in(parent)
        .map_err(|error| other(format!("Could not stage the target policy: {error}")))?;
    let archive_file = File::open(part)
        .map_err(|error| other(format!("Could not open the target policy archive: {error}")))?;
    let mut archive = tar::Archive::new(archive_file);
    let entries = archive
        .entries()
        .map_err(|error| other(format!("Could not read the target policy archive: {error}")))?;
    for entry in entries {
        check_cancel(cancel)?;
        let mut entry = entry
            .map_err(|error| other(format!("Could not read the target policy archive: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| {
                other(format!(
                    "The target policy archive has an invalid path: {error}"
                ))
            })?
            .into_owned();
        let accepted = match path.components().collect::<Vec<_>>().as_slice() {
            [Component::Normal(name)] => *name == TARGET_UKI_FILE,
            [Component::Normal(directory), Component::Normal(name)] => {
                *directory == PREDICTIONS_DIR
                    && name.to_str().is_some_and(|name| {
                        name.starts_with("generation-")
                            && (name.strip_suffix(".json").is_some()
                                || name.strip_suffix(".json.uki.sig").is_some())
                    })
            }
            _ => false,
        };
        if !accepted {
            return Err(invalid(
                "The target policy archive contains an unexpected entry.",
            ));
        }
        if !entry.header().entry_type().is_file() {
            return Err(invalid(
                "The target policy archive contains a non-file entry.",
            ));
        }
        if entry.size() > MAX_UKI {
            return Err(invalid(
                "The target policy archive entry exceeds its bound.",
            ));
        }
        let destination = staging.path().join(&path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| other(format!("Could not stage the target policy: {error}")))?;
        }
        entry
            .unpack(&destination)
            .map_err(|error| other(format!("Could not unpack the target policy: {error}")))?;
    }
    let target = std::fs::read(staging.path().join(TARGET_UKI_FILE))
        .map_err(|_| invalid("The target policy bundle has no target UKI."))?;
    if sha256(&target) != policy.target_uki_sha256 {
        return Err(invalid(
            "The target policy bundle contains a different target UKI.",
        ));
    }
    let staged = staging.keep();
    match std::fs::rename(&staged, final_root) {
        Ok(()) => {}
        Err(_) if final_root.join(TARGET_UKI_FILE).is_file() => {
            let _ = std::fs::remove_dir_all(&staged);
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staged);
            return Err(other(format!(
                "Could not place the target policy cache: {error}"
            )));
        }
    }
    Ok(())
}

fn verify_target_policy_bundle(
    root: &Path,
    policy: &TargetPolicyBundle,
) -> io::Result<TargetAcquisition> {
    let target = review_acquired_bundle(root)
        .map_err(|error| invalid(format!("The target policy bundle did not verify: {error}")))?;
    if target.generation() != policy.generation
        || target.target_uki_sha256() != policy.target_uki_sha256
    {
        return Err(invalid(
            "The target policy bundle does not match the release catalog.",
        ));
    }
    Ok(TargetAcquisition::new(target, root.to_path_buf()))
}

fn secure_target_policy_bundle(root: &Path) -> io::Result<()> {
    let secure = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| other(format!("Could not secure the target policy cache: {error}")))
    };
    secure(root, 0o700)?;
    let predictions = root.join(PREDICTIONS_DIR);
    secure(&predictions, 0o700)?;
    secure(&root.join(TARGET_UKI_FILE), 0o600)?;
    let entries = std::fs::read_dir(&predictions).map_err(|error| {
        other(format!(
            "Could not inspect the target policy cache: {error}"
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            other(format!(
                "Could not inspect the target policy cache: {error}"
            ))
        })?;
        secure(&entry.path(), 0o600)?;
    }
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)
        .map_err(|error| other(format!("Could not hash downloaded data: {error}")))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| other(format!("Could not hash downloaded data: {error}")))?;
        if read == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..read) {
            hash.update(chunk);
        }
    }
    Ok(hex::encode(hash.finalize()))
}

fn cache_root() -> PathBuf {
    tilo_paths::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-imager")
}

fn same_origin(actual: &reqwest::Url, trusted: &reqwest::Url) -> bool {
    actual.scheme() == trusted.scheme()
        && actual.host_str() == trusted.host_str()
        && actual.port_or_known_default() == trusted.port_or_known_default()
}
