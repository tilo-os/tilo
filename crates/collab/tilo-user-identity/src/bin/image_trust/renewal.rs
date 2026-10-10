//! Renew an exact image endorsement without expanding the human root's grant.
//!
//! The deployment service already owns the global update transaction. The
//! inherited descriptor retains that same open-file-description lock; closing
//! our duplicate must never explicitly unlock the parent's descriptor.

use super::{
    ImageEndorsement, PathBuf, RevocationEntry, RevocationList, canonical_bytes, decode_sha256,
    from_canonical_bytes, lock_generation_state, next_path, open_readonly_without_symlinks,
    parse_u64, read_device_key, read_generation_floor, read_private_bounded,
    required_marker_is_armed, sign_image_endorsement, usage,
    verify_image_endorsement_with_revocations_at,
};
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use tilo_user_identity::update_lock::inherit as inherited_lock;

struct Request {
    trust: PathBuf,
    device_key: PathBuf,
    image_manifest_sha256: [u8; 32],
    generation: u64,
    now: u64,
    lock_fd: RawFd,
    lock_path: PathBuf,
}

pub(super) fn run(mut args: impl Iterator<Item = std::ffi::OsString>) -> Result<(), String> {
    let request = Request {
        trust: next_path(&mut args)?,
        device_key: next_path(&mut args)?,
        image_manifest_sha256: decode_sha256(&args.next().ok_or_else(usage)?)?,
        generation: parse_u64(&args.next().ok_or_else(usage)?, "generation")?,
        now: parse_u64(&args.next().ok_or_else(usage)?, "current time")?,
        lock_fd: args
            .next()
            .ok_or_else(usage)?
            .to_str()
            .and_then(|value| value.parse::<RawFd>().ok())
            .filter(|fd| *fd >= 3)
            .ok_or("invalid inherited update lock descriptor")?,
        lock_path: next_path(&mut args)?,
    };
    if args.next().is_some() {
        return Err(usage());
    }
    renew(&request)
}

fn renew(request: &Request) -> Result<(), String> {
    let _update_lock = inherited_lock(request.lock_fd, &request.lock_path)?;
    let trust =
        open_readonly_without_symlinks(&request.trust).map_err(|error| error.to_string())?;
    let metadata = trust.metadata().map_err(|error| error.to_string())?;
    // SAFETY: geteuid has no pointer arguments and cannot fail.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err("trust directory must be caller-owned mode 0700".into());
    }
    let _generation_lock = lock_generation_state(&request.trust.join("generation.state"))?;
    if !required_marker_is_armed(&request.trust.join("required"))? {
        return Err("endorsement renewal requires an armed owner trust store".into());
    }
    let root_bytes = read_private_bounded(&request.trust.join("root.pub"))?;
    let root: [u8; 32] = root_bytes
        .try_into()
        .map_err(|_| "invalid trusted root length")?;
    let entries: Vec<RevocationEntry> = from_canonical_bytes(&read_private_bounded(
        &request.trust.join("revocations.bin"),
    )?)
    .map_err(|error| error.to_string())?;
    let mut revocations = RevocationList::new();
    for entry in entries {
        if entry.user_public_key != root {
            return Err("revocation belongs to another human root".into());
        }
        revocations
            .insert(entry)
            .map_err(|error| error.to_string())?;
    }
    let previous: ImageEndorsement = from_canonical_bytes(&read_private_bounded(
        &request.trust.join("endorsement.bin"),
    )?)
    .map_err(|error| error.to_string())?;
    let successor = signed_successor(request, previous, &root, &revocations)?;
    tilo_files::native::atomic::write_with_mode(
        &request.trust.join("endorsement.bin"),
        &canonical_bytes(&successor).map_err(|error| error.to_string())?,
        0o600,
    )
    .map_err(|error| error.to_string())
}

fn signed_successor(
    request: &Request,
    mut previous: ImageEndorsement,
    root: &[u8; 32],
    revocations: &RevocationList,
) -> Result<ImageEndorsement, String> {
    // An old endorsement may be stale, but its signature and embedded grant
    // must authenticate at issuance. Current revocations always apply, and
    // signing/verifying the successor checks the grant and certificate NOW.
    verify_image_endorsement_with_revocations_at(
        &previous,
        root,
        revocations,
        previous.issued_at_ns,
    )
    .map_err(|error| error.to_string())?;
    if request.now < previous.issued_at_ns {
        return Err("successor may not backdate the current endorsement".into());
    }
    if request.generation < previous.generation
        || (request.generation == previous.generation
            && request.image_manifest_sha256 != previous.image_manifest_sha256)
    {
        return Err("successor may not regress or replace an endorsed generation".into());
    }
    for (name, value) in [
        ("generation.state", request.generation),
        ("time.state", request.now),
    ] {
        if let Some(floor) = read_generation_floor(&request.trust.join(name))?
            && value < floor
        {
            return Err(format!("successor is below the recorded {name} floor"));
        }
    }
    previous.image_manifest_sha256 = request.image_manifest_sha256;
    previous.generation = request.generation;
    previous.issued_at_ns = request.now;
    let key = read_device_key(&request.device_key).map_err(|error| error.to_string())?;
    sign_image_endorsement(&mut previous, &key).map_err(|error| {
        format!("owner image authorization must be renewed explicitly: {error}")
    })?;
    verify_image_endorsement_with_revocations_at(&previous, root, revocations, request.now)
        .map_err(|error| error.to_string())?;
    Ok(previous)
}

#[cfg(test)]
mod tests {
    use super::{Request, inherited_lock, renew};
    use crate::{
        DeviceCert, DirBuilder, DirBuilderExt, ImageEndorsement, ImageSigningAuthorization,
        RevocationEntry, canonical_bytes, from_canonical_bytes, open_lock_without_symlinks,
        open_readonly_without_symlinks, read_device_key, read_generation_floor,
        read_private_bounded, sign_image_endorsement, sign_image_signing_authorization,
        verify_image_endorsement_at,
    };
    use std::os::fd::AsRawFd;
    use tilo_user_identity::{DeviceCapabilities, DeviceIdentity, UserIdentity, sign_device_cert};

    struct Fixture {
        directory: tempfile::TempDir,
        request: Request,
        _lock: advisory_lock::FileLock,
        root_key: ed25519_dalek::SigningKey,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let trust = directory.path().join("trust");
        DirBuilder::new().mode(0o700).create(&trust).unwrap();
        let (root, user) = UserIdentity::generate("fixture", 1);
        let (device, identity) = DeviceIdentity::generate("fixture-device", 2);
        let mut cert = DeviceCert {
            user_public_key: user.public_key,
            device_public_key: identity.public_key,
            device_label: identity.label,
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 3,
            expires_at_ns: Some(100),
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let mut grant = ImageSigningAuthorization {
            user_public_key: user.public_key,
            device_public_key: identity.public_key,
            issued_at_ns: 4,
            expires_at_ns: 90,
            signature: [0; 64],
        };
        sign_image_signing_authorization(&mut grant, &root).unwrap();
        let mut old = ImageEndorsement {
            image_manifest_sha256: [1; 32],
            generation: 1,
            issued_at_ns: 5,
            device_certificate: cert,
            signing_authorization: grant,
            signature: [0; 64],
        };
        sign_image_endorsement(&mut old, &device).unwrap();
        let device_key = directory.path().join("device.key");
        tilo_user_identity::persist::write_device_key(&device_key, &device).unwrap();
        for (name, bytes) in [
            ("root.pub", user.public_key.to_vec()),
            (
                "revocations.bin",
                canonical_bytes(&Vec::<RevocationEntry>::new()).unwrap(),
            ),
            ("endorsement.bin", canonical_bytes(&old).unwrap()),
            ("generation.state", 1_u64.to_le_bytes().to_vec()),
            ("time.state", 5_u64.to_le_bytes().to_vec()),
            ("required", b"armed\n".to_vec()),
        ] {
            tilo_files::native::atomic::write_with_mode(&trust.join(name), &bytes, 0o600).unwrap();
        }
        let lock_path = directory.path().join("update.lock");
        let file = open_lock_without_symlinks(&lock_path).unwrap();
        let lock_fd = file.as_raw_fd();
        let lock = advisory_lock::FileLock::exclusive(file).unwrap();
        Fixture {
            directory,
            _lock: lock,
            root_key: root,
            request: Request {
                trust,
                device_key,
                image_manifest_sha256: [2; 32],
                generation: 2,
                now: 50,
                lock_fd,
                lock_path,
            },
        }
    }

    #[test]
    fn renewal_preserves_owner_grant_floors_and_parent_transaction() {
        let fixture = fixture();
        let trust = fixture.request.trust.clone();
        let original = read_private_bounded(&trust.join("endorsement.bin")).unwrap();
        let old: ImageEndorsement = from_canonical_bytes(&original).unwrap();
        let lock_path = fixture.request.lock_path.clone();
        renew(&fixture.request).unwrap();
        let current: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        assert_eq!(current.generation, 2);
        assert_eq!(current.image_manifest_sha256, [2; 32]);
        assert_eq!(current.signing_authorization, old.signing_authorization);
        assert_eq!(current.device_certificate, old.device_certificate);
        assert_eq!(
            read_generation_floor(&trust.join("generation.state")).unwrap(),
            Some(1)
        );
        assert_eq!(
            read_generation_floor(&trust.join("time.state")).unwrap(),
            Some(5)
        );
        let probe = open_readonly_without_symlinks(&lock_path).unwrap();
        assert!(matches!(
            advisory_lock::FileLock::try_exclusive(probe),
            Err(advisory_lock::TryLockError::WouldBlock)
        ));
        verify_image_endorsement_at(&current, &old.device_certificate.user_public_key, 50).unwrap();
    }

    #[test]
    fn exact_previously_endorsed_successor_can_retry_without_expanding_authority() {
        let mut fixture = fixture();
        renew(&fixture.request).unwrap();
        let trust = fixture.request.trust.clone();
        let previous: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        fixture.request.now = 51;
        renew(&fixture.request).unwrap();
        let current: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        assert_eq!(current.generation, previous.generation);
        assert_eq!(
            current.image_manifest_sha256,
            previous.image_manifest_sha256
        );
        assert_eq!(
            current.signing_authorization,
            previous.signing_authorization
        );
        assert_eq!(current.device_certificate, previous.device_certificate);
        fixture.request.image_manifest_sha256 = [9; 32];
        assert!(renew(&fixture.request).is_err());
        let retained: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        assert_eq!(retained, current);
    }

    #[test]
    fn renewal_refuses_expired_grant_rollback_and_backdating_without_writes() {
        for (generation, now) in [(2, 90), (1, 50), (2, 4)] {
            let mut fixture = fixture();
            let original =
                read_private_bounded(&fixture.request.trust.join("endorsement.bin")).unwrap();
            let trust = fixture.request.trust.clone();
            fixture.request.generation = generation;
            fixture.request.now = now;
            assert!(renew(&fixture.request).is_err());
            assert_eq!(
                read_private_bounded(&trust.join("endorsement.bin")).unwrap(),
                original
            );
        }
    }

    #[test]
    fn inherited_transaction_refuses_unlocked_unrelated_and_redirected_descriptors() {
        let fixture = fixture();
        let unrelated =
            open_lock_without_symlinks(&fixture.directory.path().join("unrelated.lock")).unwrap();
        assert!(inherited_lock(unrelated.as_raw_fd(), &fixture.request.lock_path).is_err());
        let fresh_path = fixture.directory.path().join("fresh.lock");
        let unlocked = open_lock_without_symlinks(&fresh_path).unwrap();
        assert!(inherited_lock(unlocked.as_raw_fd(), &fresh_path).is_err());
        let link = fixture.directory.path().join("redirected.lock");
        std::os::unix::fs::symlink(&fixture.request.lock_path, &link).unwrap();
        assert!(inherited_lock(fixture.request.lock_fd, &link).is_err());
    }
    #[test]
    fn renewal_rechecks_current_revocations_and_refuses_forged_previous_authority() {
        use tilo_user_identity::{RevocationReason, sign_revocation_entry};
        for revoke in [true, false] {
            let fixture = fixture();
            let trust = fixture.request.trust.clone();
            let mut old: ImageEndorsement = from_canonical_bytes(
                &read_private_bounded(&trust.join("endorsement.bin")).unwrap(),
            )
            .unwrap();
            if revoke {
                let mut entry = RevocationEntry {
                    user_public_key: old.device_certificate.user_public_key,
                    revoked_device_public_key: old.device_certificate.device_public_key,
                    revoked_at_ns: 45,
                    reason: RevocationReason::Lost,
                    signature: [0; 64],
                };
                sign_revocation_entry(&mut entry, &fixture.root_key).unwrap();
                tilo_files::native::atomic::write_with_mode(
                    &trust.join("revocations.bin"),
                    &canonical_bytes(&vec![entry]).unwrap(),
                    0o600,
                )
                .unwrap();
            } else {
                old.signature[0] ^= 1;
                tilo_files::native::atomic::write_with_mode(
                    &trust.join("endorsement.bin"),
                    &canonical_bytes(&old).unwrap(),
                    0o600,
                )
                .unwrap();
            }
            let before = read_private_bounded(&trust.join("endorsement.bin")).unwrap();
            assert!(renew(&fixture.request).is_err());
            assert_eq!(
                read_private_bounded(&trust.join("endorsement.bin")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn stale_endorsement_can_use_still_current_grant_without_extending_its_expiry() {
        let mut fixture = fixture();
        let trust = fixture.request.trust.clone();
        let mut old: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        let expiry = tilo_user_identity::MAX_IMAGE_ENDORSEMENT_AGE_NS * 2;
        old.device_certificate.expires_at_ns = Some(expiry);
        sign_device_cert(&mut old.device_certificate, &fixture.root_key).unwrap();
        old.signing_authorization.expires_at_ns = expiry;
        sign_image_signing_authorization(&mut old.signing_authorization, &fixture.root_key)
            .unwrap();
        let device = read_device_key(&fixture.request.device_key).unwrap();
        sign_image_endorsement(&mut old, &device).unwrap();
        fixture.request.now = tilo_user_identity::MAX_IMAGE_ENDORSEMENT_AGE_NS + 6;
        assert_eq!(
            verify_image_endorsement_at(
                &old,
                &old.device_certificate.user_public_key,
                fixture.request.now
            ),
            Err(tilo_user_identity::ImageEndorsementError::TooOld)
        );
        tilo_files::native::atomic::write_with_mode(
            &trust.join("endorsement.bin"),
            &canonical_bytes(&old).unwrap(),
            0o600,
        )
        .unwrap();
        renew(&fixture.request).unwrap();
        let current: ImageEndorsement =
            from_canonical_bytes(&read_private_bounded(&trust.join("endorsement.bin")).unwrap())
                .unwrap();
        assert_eq!(current.signing_authorization, old.signing_authorization);
        assert_eq!(current.signing_authorization.expires_at_ns, expiry);
    }
}
