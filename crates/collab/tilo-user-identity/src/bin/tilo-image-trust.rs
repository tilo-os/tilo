//! Offline tooling for the human-root -> device -> immutable-image trust chain.

use std::{
    fs::{DirBuilder, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use tilo_user_identity::{
    DeviceCert, ImageEndorsement, ImageSigningAuthorization,
    MAX_IMAGE_SIGNING_AUTHORIZATION_LIFETIME_NS, RevocationEntry, RevocationList,
    canonical::{canonical_bytes, from_canonical_bytes},
    image_endorsement_signing_digest, image_signing_authorization_digest,
    persist::{read_device_cert, read_device_key, read_owned_signing_key},
    sign_image_endorsement, sign_image_signing_authorization, verify_device_cert_at,
    verify_image_endorsement_at, verify_image_endorsement_with_revocations_at,
    verify_image_signing_authorization_at, verify_revocation_entry,
};

#[path = "image_trust/renewal.rs"]
mod renewal;

const MAX_ARTIFACT_BYTES: u64 = 64 * 1024;

#[cfg(target_os = "linux")]
fn open_without_symlinks(
    path: &Path,
    flags: libc::c_int,
    mode: libc::mode_t,
) -> std::io::Result<std::fs::File> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "artifact path contains an embedded NUL byte",
        )
    })?;
    tilo_user_identity::nofollow::open(None, &path, flags, mode, false)
}

/// Only Linux's `openat2` refuses symlinks at every step of the path, so
/// elsewhere the tool opens no artifact.
#[cfg(not(target_os = "linux"))]
fn open_without_symlinks(
    _path: &Path,
    _flags: libc::c_int,
    _mode: libc::mode_t,
) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening an artifact without following symlinks needs Linux's openat2",
    ))
}

fn open_readonly_without_symlinks(path: &Path) -> std::io::Result<std::fs::File> {
    open_without_symlinks(path, libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK, 0)
}

fn open_lock_without_symlinks(path: &Path) -> std::io::Result<std::fs::File> {
    open_without_symlinks(
        path,
        libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC | libc::O_NONBLOCK,
        0o600,
    )
}

fn usage() -> String {
    "usage:\n  tilo-image-trust parse-bootc-status < status.json\n  tilo-image-trust bootc-status-field SELECTOR < status.json\n  tilo-image-trust grant ROOT_KEY DEVICE_CERT ISSUED_NS EXPIRES_NS OUTPUT\n  tilo-image-trust prepare-grant DEVICE_CERT ISSUED_NS EXPIRES_NS UNSIGNED_OUTPUT DIGEST_OUTPUT\n  tilo-image-trust attach-grant UNSIGNED_GRANT SIGNATURE OUTPUT\n  tilo-image-trust endorse DEVICE_KEY DEVICE_CERT GRANT IMAGE_SHA256 GENERATION ISSUED_NS OUTPUT\n  tilo-image-trust prepare-endorsement DEVICE_CERT GRANT IMAGE_SHA256 GENERATION ISSUED_NS UNSIGNED_OUTPUT DIGEST_OUTPUT\n  tilo-image-trust attach-endorsement UNSIGNED_ENDORSEMENT SIGNATURE OUTPUT\n  tilo-image-trust revocations ROOT_PUBLIC_KEY OUTPUT [REVOCATION_ENTRY ...]\n  tilo-image-trust verify ROOT_PUBLIC_KEY REVOCATIONS ENDORSEMENT IMAGE_SHA256 GENERATION NOW_NS\n  tilo-image-trust verify-and-record ROOT_PUBLIC_KEY REVOCATIONS ENDORSEMENT IMAGE_SHA256 GENERATION NOW_NS GENERATION_STATE TIME_STATE\n  tilo-image-trust arm ROOT_PUBLIC_KEY REVOCATIONS ENDORSEMENT NOW_NS TRUST_DIR [UPDATE_LOCK]\n  tilo-image-trust rotate ROOT_PUBLIC_KEY REVOCATIONS ENDORSEMENT NOW_NS TRUST_DIR [UPDATE_LOCK]\n  tilo-image-trust renew-under-update-lock TRUST_DIR DEVICE_KEY IMAGE_SHA256 GENERATION NOW_NS UPDATE_LOCK_FD UPDATE_LOCK".into()
}

fn parse_bootc_status_bytes(bytes: &[u8]) -> Result<Option<(String, String)>, String> {
    let document: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("malformed bootc status: {error}"))?;
    let staged = document
        .get("status")
        .and_then(|status| status.get("staged"))
        .ok_or("bootc status has no status.staged field")?;
    if staged.is_null() {
        return Ok(None);
    }
    let staged = staged
        .as_object()
        .ok_or("bootc status.staged is neither an object nor null")?;
    let digest = staged
        .get("image")
        .and_then(|image| image.get("imageDigest"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let verity = staged
        .get("composefs")
        .and_then(|composefs| composefs.get("verity"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    for (name, value) in [("image digest", &digest), ("composefs digest", &verity)] {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err(format!(
                "bootc staged {name} is oversized or contains control characters"
            ));
        }
    }
    Ok(Some((digest, verity)))
}

fn parse_bootc_status(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    if args.next().is_some() {
        return Err(usage());
    }
    let mut bytes = Vec::with_capacity(64 * 1024);
    std::io::stdin()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read bootc status: {error}"))?;
    if bytes.len() > 1_048_576 {
        return Err("bootc status exceeds 1 MiB".into());
    }
    if let Some((digest, verity)) = parse_bootc_status_bytes(&bytes)? {
        println!("{digest}");
        println!("{verity}");
    } else {
        println!("NONE");
    }
    Ok(())
}

fn bootc_status_field_bytes(bytes: &[u8], selector: &str) -> Result<String, String> {
    let document: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("malformed bootc status: {error}"))?;
    let status = document
        .get("status")
        .and_then(serde_json::Value::as_object)
        .ok_or("bootc status has no object-valued status field")?;
    let (deployment, path): (&str, &[&str]) = match selector {
        "booted-image" => ("booted", &["image", "image", "image"]),
        "staged-image" => ("staged", &["image", "image", "image"]),
        "rollback-image" => ("rollback", &["image", "image", "image"]),
        "booted-composefs" => ("booted", &["composefs", "verity"]),
        "staged-composefs" => ("staged", &["composefs", "verity"]),
        _ => return Err(format!("unsupported bootc status selector: {selector}")),
    };
    let Some(mut value) = status.get(deployment) else {
        return Err(format!("bootc status has no status.{deployment} field"));
    };
    if value.is_null() {
        return Ok(String::new());
    }
    if !value.is_object() {
        return Err(format!(
            "bootc status.{deployment} is neither an object nor null"
        ));
    }
    for component in path {
        let Some(next) = value.get(component) else {
            return Ok(String::new());
        };
        value = next;
    }
    let value = value.as_str().unwrap_or_default();
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err("bootc status field is oversized or contains control characters".into());
    }
    Ok(value.to_owned())
}

fn bootc_status_field(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let selector = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let mut bytes = Vec::with_capacity(64 * 1024);
    std::io::stdin()
        .take(1_048_577)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read bootc status: {error}"))?;
    if bytes.len() > 1_048_576 {
        return Err("bootc status exceeds 1 MiB".into());
    }
    println!("{}", bootc_status_field_bytes(&bytes, &selector)?);
    Ok(())
}

fn parse_u64(value: &std::ffi::OsStr, name: &str) -> Result<u64, String> {
    value
        .to_str()
        .ok_or_else(|| format!("{name} must be UTF-8"))?
        .parse()
        .map_err(|_| format!("{name} must be an unsigned 64-bit integer"))
}

fn decode_sha256(value: &std::ffi::OsStr) -> Result<[u8; 32], String> {
    let value = value
        .to_str()
        .ok_or("image digest must be UTF-8")?
        .strip_prefix("sha256:")
        .unwrap_or_else(|| value.to_str().unwrap_or_default());
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("image digest must be exactly 64 hexadecimal SHA-256 digits".into());
    }
    let mut digest = [0_u8; 32];
    for (output, pair) in digest.iter_mut().zip(value.as_bytes().chunks_exact(2)) {
        let text = std::str::from_utf8(pair).map_err(|_| "image digest is not ASCII")?;
        *output = u8::from_str_radix(text, 16)
            .map_err(|_| "image digest contains invalid hexadecimal")?;
    }
    Ok(digest)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_readonly_without_symlinks(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    if !file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .is_file()
    {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{} exceeds the artifact size limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn read_private_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let file = open_readonly_without_symlinks(path)
        .map_err(|error| format!("cannot open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    // Live trust artifacts are mutable root authority, despite containing no
    // secret material. Reject aliases and writable-by-others inodes so an
    // update cannot verify bytes controlled through another pathname.
    // SAFETY: geteuid takes no arguments and has no failure mode.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(format!(
            "{} must be caller-owned, mode 0600 or stricter, and have one link",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_ARTIFACT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{} exceeds the artifact size limit",
            path.display()
        ));
    }
    Ok(bytes)
}

fn decode_artifact<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, String> {
    from_canonical_bytes(&read_bounded(path)?).map_err(|error| error.to_string())
}

fn write_new(path: &Path, value: &impl serde::Serialize) -> Result<(), String> {
    let bytes = canonical_bytes(value).map_err(|error| error.to_string())?;
    write_new_bytes(path, &bytes)
}

fn write_new_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{} exceeds the artifact size limit",
            path.display()
        ));
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o644)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn read_signature(path: &Path) -> Result<[u8; 64], String> {
    read_bounded(path)?
        .try_into()
        .map_err(|_| "Ed25519 signature must contain exactly 64 raw bytes".into())
}

fn next_path(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<PathBuf, String> {
    args.next().map(PathBuf::from).ok_or_else(usage)
}

fn read_root_public_key(path: &Path) -> Result<[u8; 32], String> {
    read_bounded(path)?
        .try_into()
        .map_err(|_| "root public key must contain exactly 32 bytes".into())
}

fn grant(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let root_path = next_path(&mut args)?;
    let cert_path = next_path(&mut args)?;
    let issued = args.next().ok_or_else(usage)?;
    let expires = args.next().ok_or_else(usage)?;
    let issued_at_ns = parse_u64(&issued, "issued time")?;
    let expires_at_ns = parse_u64(&expires, "expiry time")?;
    let output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let root = read_owned_signing_key(&root_path).map_err(|error| error.to_string())?;
    let cert = read_device_cert(&cert_path).map_err(|error| error.to_string())?;
    verify_device_cert_at(&cert, issued_at_ns).map_err(|error| error.to_string())?;
    if root.verifying_key().to_bytes() != cert.user_public_key {
        return Err("root key does not own the supplied device certificate".into());
    }
    let mut authorization = ImageSigningAuthorization {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
    };
    sign_image_signing_authorization(&mut authorization, &root)
        .map_err(|error| error.to_string())?;
    write_new(&output, &authorization)
}

fn prepare_grant(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let cert_path = next_path(&mut args)?;
    let issued = args.next().ok_or_else(usage)?;
    let expires = args.next().ok_or_else(usage)?;
    let issued_at_ns = parse_u64(&issued, "issued time")?;
    let expires_at_ns = parse_u64(&expires, "expiry time")?;
    let output = next_path(&mut args)?;
    let digest_output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let cert = read_device_cert(&cert_path).map_err(|error| error.to_string())?;
    verify_device_cert_at(&cert, issued_at_ns).map_err(|error| error.to_string())?;
    let lifetime = expires_at_ns
        .checked_sub(issued_at_ns)
        .filter(|lifetime| *lifetime > 0)
        .ok_or("image-signing authorization has an invalid validity interval")?;
    if lifetime > MAX_IMAGE_SIGNING_AUTHORIZATION_LIFETIME_NS {
        return Err("image-signing authorization exceeds the 30-day maximum lifetime".into());
    }
    let authorization = ImageSigningAuthorization {
        user_public_key: cert.user_public_key,
        device_public_key: cert.device_public_key,
        issued_at_ns,
        expires_at_ns,
        signature: [0; 64],
    };
    let digest =
        image_signing_authorization_digest(&authorization).map_err(|error| error.to_string())?;
    write_new(&output, &authorization)?;
    write_new_bytes(&digest_output, &digest)
}

fn attach_grant(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let unsigned_path = next_path(&mut args)?;
    let signature_path = next_path(&mut args)?;
    let output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let mut authorization: ImageSigningAuthorization = decode_artifact(&unsigned_path)?;
    if authorization.signature != [0; 64] {
        return Err("grant input is already signed".into());
    }
    authorization.signature = read_signature(&signature_path)?;
    verify_image_signing_authorization_at(
        &authorization,
        &authorization.user_public_key,
        authorization.issued_at_ns,
    )
    .map_err(|error| error.to_string())?;
    write_new(&output, &authorization)
}

fn endorse(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let device_path = next_path(&mut args)?;
    let cert_path = next_path(&mut args)?;
    let grant_path = next_path(&mut args)?;
    let digest = args.next().ok_or_else(usage)?;
    let generation_arg = args.next().ok_or_else(usage)?;
    let issued = args.next().ok_or_else(usage)?;
    let image_manifest_sha256 = decode_sha256(&digest)?;
    let generation = parse_u64(&generation_arg, "generation")?;
    let issued_at_ns = parse_u64(&issued, "issued time")?;
    let output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let device = read_device_key(&device_path).map_err(|error| error.to_string())?;
    let device_certificate: DeviceCert =
        read_device_cert(&cert_path).map_err(|error| error.to_string())?;
    verify_device_cert_at(&device_certificate, issued_at_ns).map_err(|error| error.to_string())?;
    let signing_authorization = decode_artifact(&grant_path)?;
    let mut endorsement = ImageEndorsement {
        image_manifest_sha256,
        generation,
        issued_at_ns,
        device_certificate,
        signing_authorization,
        signature: [0; 64],
    };
    sign_image_endorsement(&mut endorsement, &device).map_err(|error| error.to_string())?;
    write_new(&output, &endorsement)
}

fn prepare_endorsement(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let cert_path = next_path(&mut args)?;
    let grant_path = next_path(&mut args)?;
    let digest = args.next().ok_or_else(usage)?;
    let generation_arg = args.next().ok_or_else(usage)?;
    let issued = args.next().ok_or_else(usage)?;
    let image_manifest_sha256 = decode_sha256(&digest)?;
    let generation = parse_u64(&generation_arg, "generation")?;
    let issued_at_ns = parse_u64(&issued, "issued time")?;
    let output = next_path(&mut args)?;
    let digest_output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let device_certificate = read_device_cert(&cert_path).map_err(|error| error.to_string())?;
    verify_device_cert_at(&device_certificate, issued_at_ns).map_err(|error| error.to_string())?;
    let signing_authorization: ImageSigningAuthorization = decode_artifact(&grant_path)?;
    verify_image_signing_authorization_at(
        &signing_authorization,
        &device_certificate.user_public_key,
        issued_at_ns,
    )
    .map_err(|error| error.to_string())?;
    if signing_authorization.device_public_key != device_certificate.device_public_key {
        return Err("image-signing grant names a different device certificate".into());
    }
    let endorsement = ImageEndorsement {
        image_manifest_sha256,
        generation,
        issued_at_ns,
        device_certificate,
        signing_authorization,
        signature: [0; 64],
    };
    let signing_digest =
        image_endorsement_signing_digest(&endorsement).map_err(|error| error.to_string())?;
    write_new(&output, &endorsement)?;
    write_new_bytes(&digest_output, &signing_digest)
}

fn attach_endorsement(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let unsigned_path = next_path(&mut args)?;
    let signature_path = next_path(&mut args)?;
    let output = next_path(&mut args)?;
    if args.next().is_some() {
        return Err(usage());
    }
    let mut endorsement: ImageEndorsement = decode_artifact(&unsigned_path)?;
    if endorsement.signature != [0; 64] {
        return Err("endorsement input is already signed".into());
    }
    endorsement.signature = read_signature(&signature_path)?;
    verify_image_endorsement_at(
        &endorsement,
        &endorsement.device_certificate.user_public_key,
        endorsement.issued_at_ns,
    )
    .map_err(|error| error.to_string())?;
    write_new(&output, &endorsement)
}

fn revocations(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let root_path = next_path(&mut args)?;
    let output = next_path(&mut args)?;
    let trusted_root = read_root_public_key(&root_path)?;
    let mut entries = Vec::<RevocationEntry>::new();
    for path in args.map(PathBuf::from) {
        let entry: RevocationEntry = decode_artifact(&path)?;
        if entry.user_public_key != trusted_root {
            return Err(format!(
                "revocation {} chains to a different human root",
                path.display()
            ));
        }
        let mut verified = RevocationList::new();
        verified
            .insert(entry.clone())
            .map_err(|error| format!("invalid revocation {}: {error}", path.display()))?;
        entries.push(entry);
    }
    write_new(&output, &entries)
}

fn validate_rotation_continuity(
    trust_dir: &Path,
    trusted_root: &[u8; 32],
    replacement: &[RevocationEntry],
) -> Result<(), String> {
    let installed_root = read_root_public_key(&trust_dir.join("root.pub"))?;
    if &installed_root != trusted_root {
        return Err(
            "trust rotation cannot replace the pinned human root; use an explicit root-recovery ceremony"
                .into(),
        );
    }

    let installed: Vec<RevocationEntry> = decode_artifact(&trust_dir.join("revocations.bin"))?;
    for previous in installed {
        if previous.user_public_key != installed_root {
            return Err(
                "installed revocation bundle does not chain to the pinned human root".into(),
            );
        }
        verify_revocation_entry(&previous)
            .map_err(|error| format!("installed revocation bundle is invalid: {error}"))?;
        let retained_cutoff = replacement
            .iter()
            .filter(|entry| entry.revoked_device_public_key == previous.revoked_device_public_key)
            .map(|entry| entry.revoked_at_ns)
            .min();
        if retained_cutoff.is_none_or(|cutoff| cutoff > previous.revoked_at_ns) {
            return Err(
                "trust rotation would remove or weaken an installed device revocation".into(),
            );
        }
    }
    Ok(())
}

fn required_marker_is_armed(path: &Path) -> Result<bool, String> {
    let file = match open_readonly_without_symlinks(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
    {
        return Err(
            "image-trust required marker must be caller-owned, single-link mode 0600 regular file"
                .into(),
        );
    }
    let mut contents = Vec::with_capacity(7);
    file.take(7)
        .read_to_end(&mut contents)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if contents != b"armed\n" {
        return Err("image-trust required marker has invalid contents".into());
    }
    Ok(true)
}

#[allow(
    clippy::too_many_lines,
    reason = "the validate-lock-publish ceremony stays linear so marker-last ordering is reviewable"
)]
fn install_trust(
    mut args: impl Iterator<Item = std::ffi::OsString>,
    replace_armed: bool,
) -> Result<(), String> {
    let root_path = next_path(&mut args)?;
    let revocations_path = next_path(&mut args)?;
    let endorsement_path = next_path(&mut args)?;
    let now = args.next().ok_or_else(usage)?;
    let now_ns = parse_u64(&now, "current time")?;
    let trust_dir = next_path(&mut args)?;
    let update_lock_path = args.next().map_or_else(
        || PathBuf::from("/run/tilo-verified-update.lock"),
        PathBuf::from,
    );
    if args.next().is_some() {
        return Err(usage());
    }

    if !trust_dir.exists() {
        DirBuilder::new()
            .mode(0o700)
            .create(&trust_dir)
            .map_err(|error| format!("cannot create {}: {error}", trust_dir.display()))?;
    }
    let metadata = std::fs::symlink_metadata(&trust_dir)
        .map_err(|error| format!("cannot inspect {}: {error}", trust_dir.display()))?;
    // SAFETY: geteuid takes no arguments and cannot fail.
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err("trust directory must be caller-owned mode 0700 and not a symlink".into());
    }

    let update_lock = open_lock_without_symlinks(&update_lock_path)
        .map_err(|error| format!("cannot open {}: {error}", update_lock_path.display()))?;
    let update_lock_metadata = update_lock
        .metadata()
        .map_err(|error| format!("cannot inspect update lock: {error}"))?;
    if !update_lock_metadata.is_file()
        || update_lock_metadata.uid() != unsafe { libc::geteuid() }
        || update_lock_metadata.mode() & 0o077 != 0
        || update_lock_metadata.nlink() != 1
    {
        return Err(
            "update lock must be caller-owned, mode 0600 or stricter, and have one link".into(),
        );
    }
    let _update_lock = advisory_lock::FileLock::exclusive(update_lock)
        .map_err(|error| format!("cannot lock update transaction: {error}"))?;

    let lock_path = trust_dir.join(".arm.lock");
    let lock = open_lock_without_symlinks(&lock_path)
        .map_err(|error| format!("cannot open {}: {error}", lock_path.display()))?;
    let lock_metadata = lock
        .metadata()
        .map_err(|error| format!("cannot inspect arm lock: {error}"))?;
    if !lock_metadata.is_file()
        || lock_metadata.uid() != unsafe { libc::geteuid() }
        || lock_metadata.mode() & 0o077 != 0
        || lock_metadata.nlink() != 1
    {
        return Err(
            "trust-store arm lock must be caller-owned, mode 0600 or stricter, and have one link"
                .into(),
        );
    }
    let _lock = advisory_lock::FileLock::exclusive(lock)
        .map_err(|error| format!("cannot lock trust-store activation: {error}"))?;
    // `verify-and-record` takes this same stable sibling lock before reading
    // any trust artifacts.  Holding it across validation and marker-last
    // publication prevents standalone CLI users (outside tilo-verified-update's
    // global lock) from observing a mixture of the old and new root,
    // revocations, endorsement, and rollback floors.
    let generation_state = trust_dir.join("generation.state");
    let _generation_lock = lock_generation_state(&generation_state)?;
    let required = trust_dir.join("required");
    let is_armed = required_marker_is_armed(&required)?;
    if is_armed && !replace_armed {
        return Err("trust store is already armed; use rotate for a live replacement".into());
    }
    if !is_armed && replace_armed {
        return Err("trust store is not armed; use arm for initial activation".into());
    }

    let trusted_root = read_root_public_key(&root_path)?;
    let entries: Vec<RevocationEntry> = decode_artifact(&revocations_path)?;
    let mut revocations = RevocationList::new();
    for entry in &entries {
        if entry.user_public_key != trusted_root {
            return Err("revocation bundle contains an entry from another human root".into());
        }
        revocations
            .insert(entry.clone())
            .map_err(|error| format!("revocation bundle is invalid: {error}"))?;
    }
    if replace_armed {
        validate_rotation_continuity(&trust_dir, &trusted_root, &entries)?;
    }
    let endorsement: ImageEndorsement = decode_artifact(&endorsement_path)?;
    verify_image_endorsement_with_revocations_at(&endorsement, &trusted_root, &revocations, now_ns)
        .map_err(|error| error.to_string())?;
    let recorded_generation = read_generation_floor(&generation_state)?;
    if let Some(floor) = recorded_generation
        && endorsement.generation < floor
    {
        return Err(format!(
            "endorsed generation {} is below recorded rollback floor {floor}",
            endorsement.generation
        ));
    }
    if let Some(floor) = read_generation_floor(&trust_dir.join("time.state"))?
        && now_ns < floor
    {
        return Err(format!(
            "current time {now_ns} is below recorded trusted-time floor {floor}"
        ));
    }

    tilo_files::native::atomic::write_with_mode(&trust_dir.join("root.pub"), &trusted_root, 0o600)
        .map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_with_mode(
        &trust_dir.join("revocations.bin"),
        &canonical_bytes(&entries).map_err(|error| error.to_string())?,
        0o600,
    )
    .map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_with_mode(
        &trust_dir.join("endorsement.bin"),
        &canonical_bytes(&endorsement).map_err(|error| error.to_string())?,
        0o600,
    )
    .map_err(|error| error.to_string())?;
    // Arming must establish the rollback floor immediately. Otherwise a second
    // still-valid endorsement for an older image could become the first update
    // to create generation.state and silently lower trust below the generation
    // the owner used to arm this machine. Rotation may advance but never lower
    // the existing floor.
    let next_generation = recorded_generation
        .unwrap_or(endorsement.generation)
        .max(endorsement.generation);
    tilo_files::native::atomic::write_with_mode(
        &generation_state,
        &next_generation.to_le_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_with_mode(
        &trust_dir.join("time.state"),
        &now_ns.to_le_bytes(),
        0o600,
    )
    .map_err(|error| error.to_string())?;
    // This marker switches the update path from inert to fail-closed. Publish
    // it only after every validated artifact is durable.
    tilo_files::native::atomic::write_with_mode(&required, b"armed\n", 0o600)
        .map_err(|error| error.to_string())
}

fn arm(args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    install_trust(args, false)
}

fn rotate(args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    install_trust(args, true)
}

fn verify_common(
    mut args: impl Iterator<Item = std::ffi::OsString>,
    record_generation: bool,
) -> Result<(), String> {
    let root_path = next_path(&mut args)?;
    let revocations_path = next_path(&mut args)?;
    let endorsement_path = next_path(&mut args)?;
    let digest = args.next().ok_or_else(usage)?;
    let generation = args.next().ok_or_else(usage)?;
    let now = args.next().ok_or_else(usage)?;
    let expected_digest = decode_sha256(&digest)?;
    let expected_generation = parse_u64(&generation, "generation")?;
    let now_ns = parse_u64(&now, "current time")?;
    let generation_state = if record_generation {
        Some(next_path(&mut args)?)
    } else {
        None
    };
    let time_state = if record_generation {
        Some(next_path(&mut args)?)
    } else {
        None
    };
    if args.next().is_some() {
        return Err(usage());
    }

    // Serialize the read/check/write transaction. The state itself is replaced
    // atomically below, while this stable sibling inode prevents two valid but
    // differently ordered updates from lowering the recorded floor.
    let _generation_lock = generation_state
        .as_deref()
        .map(lock_generation_state)
        .transpose()?;
    if let Some(state) = time_state.as_deref()
        && let Some(floor) = read_generation_floor(state)?
        && now_ns < floor
    {
        return Err(format!(
            "current time {now_ns} is below recorded trusted-time floor {floor}"
        ));
    }
    let read_artifact = |path: &Path| {
        if record_generation {
            read_private_bounded(path)
        } else {
            read_bounded(path)
        }
    };
    let trusted_root: [u8; 32] = read_artifact(&root_path)?
        .try_into()
        .map_err(|_| "root public key must contain exactly 32 bytes")?;
    let revocation_entries: Vec<RevocationEntry> =
        from_canonical_bytes(&read_artifact(&revocations_path)?)
            .map_err(|error| error.to_string())?;
    let mut revocations = RevocationList::new();
    for entry in revocation_entries {
        if entry.user_public_key != trusted_root {
            return Err("revocation bundle contains an entry from another human root".into());
        }
        revocations
            .insert(entry)
            .map_err(|error| format!("revocation bundle is invalid: {error}"))?;
    }
    let endorsement: ImageEndorsement = from_canonical_bytes(&read_artifact(&endorsement_path)?)
        .map_err(|error| error.to_string())?;
    verify_image_endorsement_with_revocations_at(&endorsement, &trusted_root, &revocations, now_ns)
        .map_err(|error| error.to_string())?;
    if endorsement.image_manifest_sha256 != expected_digest {
        return Err("endorsement is valid but names a different image digest".into());
    }
    if endorsement.generation != expected_generation {
        return Err("endorsement is valid but names a different image generation".into());
    }
    if let Some(state) = generation_state {
        let recorded = read_generation_floor(&state)?;
        let floor = recorded.unwrap_or(0);
        if expected_generation < floor {
            return Err(format!(
                "image generation {expected_generation} is below recorded rollback floor {floor}"
            ));
        }
        if expected_generation > floor || recorded.is_none() {
            tilo_files::native::atomic::write_with_mode(
                &state,
                &expected_generation.to_le_bytes(),
                0o600,
            )
            .map_err(|error| format!("cannot record generation in {}: {error}", state.display()))?;
        }
    }
    if let Some(state) = time_state {
        let recorded = read_generation_floor(&state)?;
        if recorded.is_none_or(|floor| now_ns > floor) {
            tilo_files::native::atomic::write_with_mode(&state, &now_ns.to_le_bytes(), 0o600)
                .map_err(|error| {
                    format!("cannot record trusted time in {}: {error}", state.display())
                })?;
        }
    }
    Ok(())
}

fn verify(args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    verify_common(args, false)
}

fn lock_generation_state(path: &Path) -> Result<advisory_lock::FileLock, String> {
    let mut lock_name = path.as_os_str().to_owned();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let file = open_lock_without_symlinks(&lock_path).map_err(|error| {
        format!(
            "cannot open generation lock {}: {error}",
            lock_path.display()
        )
    })?;
    if !file
        .metadata()
        .map_err(|error| format!("cannot inspect generation lock: {error}"))?
        .is_file()
    {
        return Err("generation lock is not a regular file".into());
    }
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect generation lock: {error}"))?;
    // SAFETY: geteuid takes no arguments and has no failure mode.
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(
            "generation lock must be caller-owned, mode 0600 or stricter, and have one link".into(),
        );
    }
    advisory_lock::FileLock::exclusive(file)
        .map_err(|error| format!("cannot lock generation state: {error}"))
}

fn read_generation_floor(path: &Path) -> Result<Option<u64>, String> {
    let file = match open_readonly_without_symlinks(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    // SAFETY: geteuid takes no arguments and has no failure mode.
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || metadata.nlink() != 1
    {
        return Err(format!(
            "{} must be caller-owned, mode 0600 or stricter, and have one link",
            path.display()
        ));
    }
    let mut encoded = [0_u8; 8];
    let mut bounded = file.take(9);
    bounded
        .read_exact(&mut encoded)
        .map_err(|_| format!("{} is not an 8-byte generation state", path.display()))?;
    let mut extra = [0_u8; 1];
    if bounded
        .read(&mut extra)
        .map_err(|error| error.to_string())?
        != 0
    {
        return Err(format!(
            "{} is not an 8-byte generation state",
            path.display()
        ));
    }
    Ok(Some(u64::from_le_bytes(encoded)))
}

fn run() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    match args.next().as_deref().and_then(std::ffi::OsStr::to_str) {
        Some("parse-bootc-status") => parse_bootc_status(args),
        Some("bootc-status-field") => bootc_status_field(args),
        Some("grant") => grant(args),
        Some("prepare-grant") => prepare_grant(args),
        Some("attach-grant") => attach_grant(args),
        Some("endorse") => endorse(args),
        Some("prepare-endorsement") => prepare_endorsement(args),
        Some("attach-endorsement") => attach_endorsement(args),
        Some("revocations") => revocations(args),
        Some("arm") => arm(args),
        Some("rotate") => rotate(args),
        Some("renew-under-update-lock") => renewal::run(args),
        Some("verify") => verify(args),
        Some("verify-and-record") => verify_common(args, true),
        _ => Err(usage()),
    }
}

#[cfg(target_os = "linux")]
fn disable_core_dumps() -> bool {
    // SAFETY: PR_SET_DUMPABLE consumes scalar arguments only.
    unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0 }
}

/// No portable equivalent; refusing keeps keys out of core dumps.
#[cfg(not(target_os = "linux"))]
fn disable_core_dumps() -> bool {
    false
}

fn main() -> std::process::ExitCode {
    // Grant and endorsement commands may load human-root or device keys. Fail
    // before parsing them unless the process can make itself non-dumpable.
    if !disable_core_dumps() {
        eprintln!("tilo-image-trust: could not disable core dumps");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = run() {
        eprintln!("tilo-image-trust: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The artifact writer refuses output the reader cannot accept; the canonical reader rejects
    /// symlinks; the live trust reader rejects exposed or aliased artifacts; the required marker must
    /// be exact, private and unaliased.
    #[test]
    fn trust_artifacts_are_written_and_read_only_as_exact_private_files() {
        // Phase: artifact_writer_refuses_output_the_reader_cannot_accept.
        {
            let temp = tempfile::tempdir().expect("temporary directory");
            let output = temp.path().join("oversized.artifact");
            let bytes = vec![0_u8; usize::try_from(MAX_ARTIFACT_BYTES).unwrap() + 1];
            let error = write_new_bytes(&output, &bytes).expect_err("oversized artifact must fail");
            assert!(error.contains("artifact size limit"));
            assert!(!output.exists());
        }

        // Phase: canonical_artifact_reader_rejects_symlinks.
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::tempdir().expect("temporary directory");
            let artifact = temp.path().join("artifact");
            let link = temp.path().join("artifact-link");
            fs::write(&artifact, b"fixture").expect("artifact fixture");
            symlink(&artifact, &link).expect("artifact symlink");
            assert!(read_bounded(&link).is_err());

            let linked_parent = temp.path().join("linked-parent");
            symlink(temp.path(), &linked_parent).expect("parent-directory symlink");
            assert!(read_bounded(&linked_parent.join("artifact")).is_err());
            assert!(read_private_bounded(&linked_parent.join("artifact")).is_err());
        }

        // Phase: live_trust_reader_rejects_exposed_or_aliased_artifacts.
        {
            let temp = tempfile::tempdir().expect("temporary directory");
            let artifact = temp.path().join("root.pub");
            tilo_files::native::atomic::write_with_mode(&artifact, &[7; 32], 0o600)
                .expect("private trust artifact");
            assert_eq!(read_private_bounded(&artifact).unwrap(), vec![7; 32]);

            fs::set_permissions(&artifact, fs::Permissions::from_mode(0o640))
                .expect("expose trust artifact");
            assert!(read_private_bounded(&artifact).is_err());
            fs::set_permissions(&artifact, fs::Permissions::from_mode(0o600))
                .expect("restore private trust artifact");
            fs::hard_link(&artifact, temp.path().join("root-alias.pub"))
                .expect("alias trust artifact");
            assert!(read_private_bounded(&artifact).is_err());
        }

        // Phase: required_marker_must_be_exact_private_and_unaliased.
        {
            let temp = tempfile::tempdir().expect("temporary trust directory");
            let marker = temp.path().join("required");
            assert!(!required_marker_is_armed(&marker).expect("absent marker"));

            tilo_files::native::atomic::write_with_mode(&marker, b"armed\n", 0o600)
                .expect("valid marker");
            assert!(required_marker_is_armed(&marker).expect("valid marker accepted"));
            let linked_parent = temp.path().join("linked-parent");
            std::os::unix::fs::symlink(temp.path(), &linked_parent).expect("marker parent symlink");
            assert!(required_marker_is_armed(&linked_parent.join("required")).is_err());

            fs::write(&marker, b"wrong\n").expect("corrupt marker contents");
            assert!(required_marker_is_armed(&marker).is_err());
            fs::write(&marker, b"armed\n").expect("restore marker contents");
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o640)).expect("expose marker");
            assert!(required_marker_is_armed(&marker).is_err());
            fs::set_permissions(&marker, fs::Permissions::from_mode(0o600))
                .expect("restore marker mode");

            fs::hard_link(&marker, temp.path().join("required-alias")).expect("alias marker");
            assert!(required_marker_is_armed(&marker).is_err());
        }
    }

    use ed25519_dalek::Signer;
    use std::fs;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceIdentity, RevocationEntry, RevocationReason, UserIdentity,
        persist::{write_device_cert, write_device_key, write_signing_key},
        sign_device_cert, sign_revocation_entry,
    };

    /// The bootc status parser requires the staged schema and its field parser uses fixed bounded
    /// selectors.
    #[test]
    fn bootc_status_is_parsed_only_through_the_staged_schema() {
        // Phase: bootc_status_parser_requires_the_staged_schema.
        {
            assert_eq!(
                parse_bootc_status_bytes(br#"{"status":{"staged":null}}"#).unwrap(),
                None
            );
            assert_eq!(
                parse_bootc_status_bytes(
                    br#"{"status":{"staged":{"image":{"imageDigest":"sha256:abc"},"composefs":{"verity":"sha512:def"}}}}"#,
                )
                .unwrap(),
                Some(("sha256:abc".into(), "sha512:def".into()))
            );
            assert!(parse_bootc_status_bytes(b"not-json").is_err());
            assert!(parse_bootc_status_bytes(br#"{"status":{}}"#).is_err());
            assert!(parse_bootc_status_bytes(br#"{"status":{"staged":false}}"#).is_err());
            assert!(
                parse_bootc_status_bytes(
                    br#"{"status":{"staged":{"image":{"imageDigest":"sha256:abc\ninjected"},"composefs":{"verity":"sha512:def"}}}}"#,
                )
                .is_err()
            );
        }

        // Phase: bootc_status_field_parser_uses_fixed_bounded_selectors.
        {
            let status = br#"{"status":{"booted":{"image":{"image":{"image":"registry/tilo:current"}},"composefs":{"verity":"sha512:booted"}},"staged":{"image":{"image":{"image":"registry/tilo:next"}},"composefs":{"verity":"sha512:staged"}},"rollback":null}}"#;
            assert_eq!(
                bootc_status_field_bytes(status, "booted-image").unwrap(),
                "registry/tilo:current"
            );
            assert_eq!(
                bootc_status_field_bytes(status, "staged-composefs").unwrap(),
                "sha512:staged"
            );
            assert_eq!(
                bootc_status_field_bytes(status, "rollback-image").unwrap(),
                ""
            );
            assert!(bootc_status_field_bytes(status, "arbitrary-expression").is_err());
        }
    }

    fn arguments(values: &[&str]) -> impl Iterator<Item = std::ffi::OsString> {
        values
            .iter()
            .map(|value| std::ffi::OsString::from(*value))
            .collect::<Vec<_>>()
            .into_iter()
    }

    /// The generation floor rejects symlinks and shared state and the generation lock rejects an
    /// aliased inode.
    #[test]
    fn generation_state_refuses_symlinks_sharing_and_aliases() {
        // Phase: generation_floor_rejects_symlinks_and_shared_state.
        {
            use std::os::unix::fs::{PermissionsExt, symlink};

            let temp = tempfile::tempdir().expect("temporary directory");
            let state = temp.path().join("generation");
            let link = temp.path().join("generation-link");
            tilo_files::native::atomic::write_with_mode(&state, &7_u64.to_le_bytes(), 0o600)
                .expect("generation fixture");
            symlink(&state, &link).expect("generation symlink");
            assert!(read_generation_floor(&link).is_err());
            let linked_parent = temp.path().join("linked-parent");
            symlink(temp.path(), &linked_parent).expect("generation parent symlink");
            assert!(read_generation_floor(&linked_parent.join("generation")).is_err());

            fs::set_permissions(&state, fs::Permissions::from_mode(0o660))
                .expect("make generation state group writable");
            assert!(read_generation_floor(&state).is_err());

            fs::set_permissions(&state, fs::Permissions::from_mode(0o600))
                .expect("restore private generation state");
            fs::hard_link(&state, temp.path().join("generation-alias"))
                .expect("alias generation state");
            assert!(read_generation_floor(&state).is_err());
        }

        // Phase: generation_lock_rejects_an_aliased_inode.
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::tempdir().expect("temporary directory");
            let linked_parent = temp.path().join("linked-parent");
            symlink(temp.path(), &linked_parent).expect("lock parent symlink");
            assert!(lock_generation_state(&linked_parent.join("other-generation")).is_err());
            assert!(!temp.path().join("other-generation.lock").exists());

            let state = temp.path().join("generation");
            drop(lock_generation_state(&state).expect("create generation lock"));
            let lock = temp.path().join("generation.lock");
            fs::hard_link(&lock, temp.path().join("generation-lock-alias"))
                .expect("alias generation lock");
            assert!(lock_generation_state(&state).is_err());
        }
    }

    /// External signers complete a grant and endorsement without key files, and trust rotation
    /// cannot roll back the root or revocations.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn trust_changes_are_signed_externally_and_never_roll_back() {
        // Phase: external_signers_complete_grant_and_endorsement_without_key_files.
        {
            let temp = tempfile::tempdir().expect("temporary directory");
            let cert_path = temp.path().join("device.cert");
            let unsigned_grant = temp.path().join("unsigned.grant");
            let grant_digest = temp.path().join("grant.digest");
            let grant_signature = temp.path().join("grant.signature");
            let grant = temp.path().join("grant");
            let unsigned_endorsement = temp.path().join("unsigned.endorsement");
            let endorsement_digest = temp.path().join("endorsement.digest");
            let endorsement_signature = temp.path().join("endorsement.signature");
            let endorsement_path = temp.path().join("endorsement");
            let (root, user) = UserIdentity::generate("Ada", 10);
            let (device, device_identity) = DeviceIdentity::generate("builder", 11);
            let mut cert = DeviceCert {
                user_public_key: user.public_key,
                device_public_key: device_identity.public_key,
                device_label: device_identity.label,
                capabilities: DeviceCapabilities::member(),
                issued_at_ns: 12,
                expires_at_ns: Some(100),
                signature: [0; 64],
            };
            sign_device_cert(&mut cert, &root).expect("sign device cert");
            write_device_cert(&cert_path, &cert).expect("write device cert");

            prepare_grant(arguments(&[
                cert_path.to_str().unwrap(),
                "15",
                "80",
                unsigned_grant.to_str().unwrap(),
                grant_digest.to_str().unwrap(),
            ]))
            .expect("prepare grant");
            fs::write(
                &grant_signature,
                root.sign(&fs::read(&grant_digest).unwrap()).to_bytes(),
            )
            .unwrap();
            attach_grant(arguments(&[
                unsigned_grant.to_str().unwrap(),
                grant_signature.to_str().unwrap(),
                grant.to_str().unwrap(),
            ]))
            .expect("attach externally produced root signature");

            prepare_endorsement(arguments(&[
                cert_path.to_str().unwrap(),
                grant.to_str().unwrap(),
                "abababababababababababababababababababababababababababababababab",
                "7",
                "20",
                unsigned_endorsement.to_str().unwrap(),
                endorsement_digest.to_str().unwrap(),
            ]))
            .expect("prepare endorsement");
            fs::write(
                &endorsement_signature,
                device
                    .sign(&fs::read(&endorsement_digest).unwrap())
                    .to_bytes(),
            )
            .unwrap();
            attach_endorsement(arguments(&[
                unsigned_endorsement.to_str().unwrap(),
                endorsement_signature.to_str().unwrap(),
                endorsement_path.to_str().unwrap(),
            ]))
            .expect("attach externally produced device signature");
            let endorsement: ImageEndorsement = decode_artifact(&endorsement_path).unwrap();
            verify_image_endorsement_at(&endorsement, &user.public_key, 20)
                .expect("external signing ceremony produces a valid complete chain");
            assert!(!temp.path().join("root.key").exists());
            assert!(!temp.path().join("device.key").exists());
        }

        // Phase: trust_rotation_cannot_rollback_root_or_revocations.
        {
            let temp = tempfile::tempdir().expect("temporary directory");
            let trust_dir = temp.path();
            let (root, user) = UserIdentity::generate("Ada", 10);
            fs::write(trust_dir.join("root.pub"), user.public_key).expect("installed root");

            let mut installed = RevocationEntry {
                user_public_key: user.public_key,
                revoked_device_public_key: [42; 32],
                revoked_at_ns: 50,
                reason: RevocationReason::Compromised,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut installed, &root).expect("sign installed revocation");
            fs::write(
                trust_dir.join("revocations.bin"),
                canonical_bytes(&vec![installed.clone()]).expect("encode installed revocations"),
            )
            .expect("installed revocations");

            assert!(
                validate_rotation_continuity(trust_dir, &user.public_key, &[]).is_err(),
                "an empty replacement must not un-revoke a device"
            );
            let mut weakened = installed.clone();
            weakened.revoked_at_ns = 60;
            sign_revocation_entry(&mut weakened, &root).expect("sign weakened cutoff");
            assert!(
                validate_rotation_continuity(trust_dir, &user.public_key, &[weakened]).is_err(),
                "a later cutoff must not restore signatures from the revoked interval"
            );
            let mut strengthened = installed;
            strengthened.revoked_at_ns = 40;
            sign_revocation_entry(&mut strengthened, &root).expect("sign earlier cutoff");
            validate_rotation_continuity(trust_dir, &user.public_key, &[strengthened])
                .expect("an earlier revocation cutoff is monotonic");

            let (_other_root, other_user) = UserIdentity::generate("Mallory", 11);
            assert!(
                validate_rotation_continuity(trust_dir, &other_user.public_key, &[]).is_err(),
                "ordinary rotation must not replace the pinned human root"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end ceremony keeps the generated keys and artifacts in a single fixture"
    )]
    fn operational_chain_binds_digest_and_generation() {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root_path = temp.path().join("root.key");
        let root_public_path = temp.path().join("root.pub");
        let device_path = temp.path().join("device.key");
        let cert_path = temp.path().join("device.cert");
        let grant_path = temp.path().join("image.grant");
        let revocations_path = temp.path().join("revocations");
        let revoked_path = temp.path().join("revoked");
        let revocation_entry_path = temp.path().join("revocation.entry");
        let endorsement_path = temp.path().join("image.endorsement");
        let invalid_endorsement_path = temp.path().join("invalid-image.endorsement");
        let generation_state_path = temp.path().join("accepted-generation");
        let time_state_path = temp.path().join("accepted-time");
        let trust_dir = temp.path().join("image-trust");
        let update_lock = temp.path().join("update.lock");
        let (root, user) = UserIdentity::generate("Ada", 10);
        let (device, device_identity) = DeviceIdentity::generate("builder", 11);
        write_signing_key(&root_path, &root).expect("write root key");
        write_device_key(&device_path, &device).expect("write device key");
        fs::write(&root_public_path, user.public_key).expect("write root public key");
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: device_identity.public_key,
            device_label: device_identity.label,
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 12,
            expires_at_ns: Some(100),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).expect("sign device cert");
        write_device_cert(&cert_path, &cert).expect("write cert");

        grant(arguments(&[
            root_path.to_str().expect("root path"),
            cert_path.to_str().expect("cert path"),
            "15",
            "80",
            grant_path.to_str().expect("grant path"),
        ]))
        .expect("grant image authority");
        revocations(arguments(&[
            root_public_path.to_str().expect("public path"),
            revocations_path.to_str().expect("revocations path"),
        ]))
        .expect("create empty revocation bundle");
        let digest = "abababababababababababababababababababababababababababababababab";
        endorse(arguments(&[
            device_path.to_str().expect("device path"),
            cert_path.to_str().expect("cert path"),
            grant_path.to_str().expect("grant path"),
            digest,
            "7",
            "20",
            endorsement_path.to_str().expect("endorsement path"),
        ]))
        .expect("endorse image");
        arm(arguments(&[
            root_public_path.to_str().expect("public path"),
            revocations_path.to_str().expect("revocations path"),
            endorsement_path.to_str().expect("endorsement path"),
            "50",
            trust_dir.to_str().expect("trust directory"),
            update_lock.to_str().expect("update lock"),
        ]))
        .expect("atomically arm owner image trust");
        assert_eq!(
            fs::read(trust_dir.join("required")).expect("required marker"),
            b"armed\n"
        );
        for artifact in [
            "root.pub",
            "revocations.bin",
            "endorsement.bin",
            "generation.state",
            "time.state",
            "required",
        ] {
            let mode = fs::metadata(trust_dir.join(artifact))
                .expect("armed artifact metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(
            fs::read(trust_dir.join("generation.state")).expect("armed generation floor"),
            7_u64.to_le_bytes()
        );
        assert_eq!(
            fs::read(trust_dir.join("time.state")).expect("armed trusted-time floor"),
            50_u64.to_le_bytes()
        );
        assert!(
            arm(arguments(&[
                root_public_path.to_str().expect("public path"),
                revocations_path.to_str().expect("revocations path"),
                endorsement_path.to_str().expect("endorsement path"),
                "50",
                trust_dir.to_str().expect("trust directory"),
                update_lock.to_str().expect("update lock"),
            ]))
            .is_err(),
            "an armed store must reject piecemeal live replacement"
        );
        let installed_endorsement = fs::read(trust_dir.join("endorsement.bin"))
            .expect("installed endorsement before rotation");
        let mut invalid_endorsement: ImageEndorsement =
            decode_artifact(&endorsement_path).expect("decode endorsement fixture");
        invalid_endorsement.generation = 8;
        fs::write(
            &invalid_endorsement_path,
            canonical_bytes(&invalid_endorsement).expect("encode invalid endorsement"),
        )
        .expect("write invalid endorsement");
        assert!(
            rotate(arguments(&[
                root_public_path.to_str().expect("public path"),
                revocations_path.to_str().expect("revocations path"),
                invalid_endorsement_path
                    .to_str()
                    .expect("invalid endorsement path"),
                "50",
                trust_dir.to_str().expect("trust directory"),
                update_lock.to_str().expect("update lock"),
            ]))
            .is_err(),
            "rotation must validate the full replacement before publication"
        );
        assert_eq!(
            fs::read(trust_dir.join("endorsement.bin")).expect("endorsement after rejection"),
            installed_endorsement
        );
        rotate(arguments(&[
            root_public_path.to_str().expect("public path"),
            revocations_path.to_str().expect("revocations path"),
            endorsement_path.to_str().expect("endorsement path"),
            "50",
            trust_dir.to_str().expect("trust directory"),
            update_lock.to_str().expect("update lock"),
        ]))
        .expect("rotate an armed trust store under the update lock");
        tilo_files::native::atomic::write_with_mode(
            &trust_dir.join("generation.state"),
            &8_u64.to_le_bytes(),
            0o600,
        )
        .expect("advance armed-store rollback floor");
        assert!(
            rotate(arguments(&[
                root_public_path.to_str().expect("public path"),
                revocations_path.to_str().expect("revocations path"),
                endorsement_path.to_str().expect("endorsement path"),
                "50",
                trust_dir.to_str().expect("trust directory"),
                update_lock.to_str().expect("update lock"),
            ]))
            .is_err(),
            "rotation must preserve the accepted generation floor"
        );
        verify(arguments(&[
            root_public_path.to_str().expect("public path"),
            revocations_path.to_str().expect("revocations path"),
            endorsement_path.to_str().expect("endorsement path"),
            digest,
            "7",
            "50",
        ]))
        .expect("verify exact image");
        for artifact in [&root_public_path, &revocations_path, &endorsement_path] {
            fs::set_permissions(artifact, fs::Permissions::from_mode(0o600))
                .expect("make live trust artifact private");
        }
        verify_common(
            arguments(&[
                root_public_path.to_str().expect("root path"),
                revocations_path.to_str().expect("revocations path"),
                endorsement_path.to_str().expect("endorsement path"),
                digest,
                "7",
                "50",
                generation_state_path
                    .to_str()
                    .expect("generation state path"),
                time_state_path.to_str().expect("time state path"),
            ]),
            true,
        )
        .expect("verify and record exact image");
        assert_eq!(
            fs::read(&generation_state_path).expect("read generation state"),
            7_u64.to_le_bytes()
        );
        assert_eq!(
            fs::read(&time_state_path).expect("read trusted time state"),
            50_u64.to_le_bytes()
        );
        tilo_files::native::atomic::write_with_mode(
            &generation_state_path,
            &8_u64.to_le_bytes(),
            0o600,
        )
        .expect("advance generation state");
        assert!(
            verify_common(
                arguments(&[
                    root_public_path.to_str().expect("root path"),
                    revocations_path.to_str().expect("revocations path"),
                    endorsement_path.to_str().expect("endorsement path"),
                    digest,
                    "7",
                    "50",
                    generation_state_path
                        .to_str()
                        .expect("generation state path"),
                    time_state_path.to_str().expect("time state path"),
                ]),
                true,
            )
            .is_err(),
            "a valid older endorsement must not cross the recorded rollback floor"
        );
        tilo_files::native::atomic::write_with_mode(&time_state_path, &60_u64.to_le_bytes(), 0o600)
            .expect("advance trusted-time state");
        let clock_error = verify_common(
            arguments(&[
                root_public_path.to_str().expect("root path"),
                revocations_path.to_str().expect("revocations path"),
                endorsement_path.to_str().expect("endorsement path"),
                digest,
                "7",
                "50",
                generation_state_path
                    .to_str()
                    .expect("generation state path"),
                time_state_path.to_str().expect("time state path"),
            ]),
            true,
        )
        .expect_err("clock rollback must not resurrect older signed authority");
        assert!(clock_error.contains("trusted-time floor"));
        assert!(
            verify(arguments(&[
                root_public_path.to_str().expect("public path"),
                revocations_path.to_str().expect("revocations path"),
                endorsement_path.to_str().expect("endorsement path"),
                digest,
                "8",
                "50",
            ]))
            .is_err(),
            "a valid signature must not authorize a different generation"
        );

        let mut entry = RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: device_identity.public_key,
            revoked_at_ns: 40,
            reason: RevocationReason::Compromised,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).expect("sign revocation");
        fs::write(
            &revocation_entry_path,
            canonical_bytes(&entry).expect("encode revocation"),
        )
        .expect("write revocation");
        revocations(arguments(&[
            root_public_path.to_str().expect("public path"),
            revoked_path.to_str().expect("revoked bundle path"),
            revocation_entry_path.to_str().expect("revocation path"),
        ]))
        .expect("create revoked bundle");
        assert!(
            verify(arguments(&[
                root_public_path.to_str().expect("public path"),
                revoked_path.to_str().expect("revoked bundle path"),
                endorsement_path.to_str().expect("endorsement path"),
                digest,
                "7",
                "50",
            ]))
            .is_err(),
            "a current revocation must reject even a backdated endorsement"
        );
    }
}
