//! One-time, identity-bound import of the old global device and room hints.
//!
//! A legacy key is only a discovery hint, never membership evidence. The
//! destination is an account-owned person directory; certificates and permanent
//! root-scoped revocations decide which hints may enter it. An existing list,
//! including an empty one, is never replaced or augmented by this import.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::{Authority, ChainPolicy, DeviceCert, RevocationList, verify_chain};

const MAX_BYTES: u64 = 64 * 1024;
const MAX_PEERS: usize = 128;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn open(path: &Path, flags: i32, mode: u32) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| invalid("peer path contains NUL"))?;
        crate::nofollow::open(
            None,
            &path,
            flags | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
            false,
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, flags, mode);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "peer migration requires no-follow paths",
        ))
    }
}

fn trusted_file(file: &File, owner: u32) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || (metadata.uid() != 0 && metadata.uid() != owner)
        || metadata.mode() & 0o022 != 0
    {
        return Err(invalid("peer migration file is not private to its owner"));
    }
    Ok(())
}

/// Import certified, currently valid personal devices into a missing scoped list.
///
/// `directory` must be the calling account's directory named for `identity`
/// (an optional 16-hex suffix is allowed). Uses the same exclusive list lock as
/// enrollment and pruning, then atomically publishes mode 0640. The legacy file
/// remains available for other people to migrate independently, but is never
/// used as an authorization fallback. Revocations must already be durably
/// recorded by the caller beside the destination list.
///
/// # Errors
/// Refuses unsafe paths, owners, modes, links, oversized or malformed legacy
/// lists, or a destination that cannot be durably published.
pub fn migrate_legacy_personal_peers(
    legacy: &Path,
    directory: &Path,
    identity: &[u8; 32],
    certificates: &[DeviceCert],
    revocations: &RevocationList,
    now_ns: u64,
) -> io::Result<bool> {
    let owner = check_person_directory(directory, identity)?;
    let id = hex::encode(identity);
    let destination = directory.join("personal-device-peers.json");
    let lock_path = directory.join("personal-device-peers.json.lock");
    let lock = open(&lock_path, libc::O_RDWR | libc::O_CREAT, 0o600)?;
    trusted_file(&lock, owner)?;
    let _lock = advisory_lock::FileLock::exclusive(lock)?;
    // Even an unusable existing inode is not permission to replace a person's
    // enrollment decisions. In particular, removals must not be resurrected.
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let Some(bytes) = read_trusted(legacy, owner, MAX_BYTES)? else {
        return Ok(false);
    };
    let hints: Vec<String> =
        serde_json::from_slice(&bytes).map_err(|_| invalid("legacy peer list is invalid"))?;
    if hints.len() > MAX_PEERS {
        return Err(invalid("legacy peer list is full"));
    }
    let policy = ChainPolicy {
        user_public_key: identity,
        authority: Authority::Person,
        revocations,
        now_ns,
    };
    let mut devices = std::collections::BTreeSet::new();
    for hint in hints {
        if hint.len() != 64 || hint.bytes().any(|b| b.is_ascii_uppercase()) {
            continue;
        }
        let Some(device) = hex::decode(hint)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        else {
            continue;
        };
        if certificates
            .iter()
            .any(|cert| cert.device_public_key == device && verify_chain(cert, &policy).is_ok())
        {
            devices.insert(device);
        }
    }
    let peers: Vec<_> = devices
        .into_iter()
        .map(|device| serde_json::json!({"identity": id, "device": hex::encode(device)}))
        .collect();
    let bytes = serde_json::to_vec_pretty(&peers).map_err(io::Error::other)?;
    tilo_files::native::atomic::write_with_mode(&destination, &bytes, 0o640)?;
    Ok(true)
}

fn check_person_directory(directory: &Path, identity: &[u8; 32]) -> io::Result<u32> {
    // SAFETY: geteuid has no preconditions.
    let owner = unsafe { libc::geteuid() };
    let id = hex::encode(identity);
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let named = name == id
        || name.strip_prefix(&format!("{id}.")).is_some_and(|suffix| {
            suffix.len() == 16
                && suffix
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
    let dir = open(directory, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let metadata = dir.metadata()?;
    if !named || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(invalid(
            "peer migration destination is not this account's person directory",
        ));
    }
    Ok(owner)
}

fn read_trusted(path: &Path, owner: u32, limit: u64) -> io::Result<Option<Vec<u8>>> {
    let source = match open(path, libc::O_RDONLY, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    trusted_file(&source, owner)?;
    if source.metadata()?.len() > limit {
        return Err(invalid("legacy artifact exceeds its bound"));
    }
    let mut bytes = Vec::new();
    source
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("legacy artifact exceeds its bound"));
    }
    Ok(Some(bytes))
}

/// Evidence binding the old machine-global handoff to the selected person.
#[cfg(target_os = "linux")]
pub struct LegacyRoomBinding<'a> {
    /// Selected personal root, never inferred from the global list.
    pub identity: &'a [u8; 32],
    /// Old machine's protected root key and default device certificate.
    pub paths: &'a crate::IdentityPaths,
    /// Certificates loaded from this account's fleet.
    pub certificates: &'a [DeviceCert],
    /// Verified fleet and durable scoped revocations.
    pub revocations: &'a RevocationList,
    /// Certificate validity clock, in nanoseconds since the Unix epoch.
    pub now_ns: u64,
}

/// Import room discovery hints only for the old machine's certified person.
///
/// The root key must agree with the selected identity, and either the default
/// device certificate or an account fleet certificate must verify for that
/// root. All source artifacts are bounded, owner-checked and opened without
/// following any symlink. Public hints need not be secret, but only root or
/// this account may own them and no other account may write them.
///
/// `validate` must apply the consumer's bounded room schema and return encoded
/// bytes. Publication uses the same lock and mode as `PersonPeers` room writers;
/// any existing inode (including an empty list) wins. These hints grant no room
/// authority: ordinary governance admission must still precede replication.
///
/// # Errors
/// Refuses unbound, unsafe or malformed artifacts and publication failures.
#[cfg(target_os = "linux")]
pub fn migrate_legacy_personal_rooms(
    legacy: &Path,
    directory: &Path,
    binding: &LegacyRoomBinding<'_>,
    validate: impl FnOnce(&[u8]) -> io::Result<Vec<u8>>,
) -> io::Result<bool> {
    let owner = check_person_directory(directory, binding.identity)?;
    let destination = directory.join("personal-rooms.json");
    let lock = open(
        &directory.join("personal-rooms.json.lock"),
        libc::O_RDWR | libc::O_CREAT,
        0o600,
    )?;
    trusted_file(&lock, owner)?;
    let _lock = advisory_lock::FileLock::exclusive(lock)?;
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => return Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let Some(bytes) = read_trusted(legacy, owner, MAX_BYTES)? else {
        return Ok(false);
    };
    let key = read_trusted(&binding.paths.user_pubkey, owner, 32)?;
    if key.as_deref() != Some(binding.identity.as_slice()) {
        return Err(invalid("legacy rooms belong to another or unknown person"));
    }
    let policy = ChainPolicy {
        user_public_key: binding.identity,
        authority: Authority::Person,
        revocations: binding.revocations,
        now_ns: binding.now_ns,
    };
    let default = read_trusted(&binding.paths.device_cert, owner, MAX_BYTES)?
        .map(|bytes| crate::canonical::from_canonical_bytes::<DeviceCert>(&bytes))
        .transpose()
        .map_err(io::Error::other)?;
    if !default
        .iter()
        .chain(binding.certificates)
        .any(|cert| verify_chain(cert, &policy).is_ok())
    {
        return Err(invalid(
            "legacy room identity has no valid device certificate",
        ));
    }
    let bytes = validate(&bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("room list exceeds its bound"));
    }
    tilo_files::native::atomic::write_with_mode(&destination, &bytes, 0o640)?;
    Ok(true)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{
        DeviceCapabilities, RevocationEntry, RevocationReason, sign_device_cert,
        sign_revocation_entry,
    };
    use ed25519_dalek::SigningKey;
    use std::os::unix::fs::PermissionsExt;

    fn cert(root: &SigningKey, key: u8) -> DeviceCert {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: SigningKey::from_bytes(&[key; 32])
                .verifying_key()
                .to_bytes(),
            device_label: "device".into(),
            capabilities: DeviceCapabilities::joined(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        cert
    }

    fn person(peers: &Path, root: &SigningKey) -> std::path::PathBuf {
        let dir = peers.join(hex::encode(root.verifying_key().to_bytes()));
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o2750)).unwrap();
        dir
    }

    fn devices(dir: &Path) -> Vec<String> {
        let list: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(dir.join("personal-device-peers.json")).unwrap())
                .unwrap();
        list.into_iter()
            .map(|peer| peer["device"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn room_import_requires_root_binding_and_safe_account_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let foreign = SigningKey::from_bytes(&[2; 32]);
        let identity = root.verifying_key().to_bytes();
        let directory = person(temp.path(), &root);
        let paths = crate::IdentityPaths::under(temp.path());
        paths.ensure_directories().unwrap();
        crate::persist::write_public_key(&paths.user_pubkey, &identity).unwrap();
        let valid = cert(&root, 3);
        crate::persist::write_device_cert(&paths.device_cert, &valid).unwrap();
        let legacy = temp.path().join("personal-rooms.json");
        let bytes = br#"["11111111-1111-4111-8111-111111111111"]"#;
        std::fs::write(&legacy, bytes).unwrap();
        let revocations = RevocationList::new();
        let binding = LegacyRoomBinding {
            identity: &identity,
            paths: &paths,
            certificates: &[],
            revocations: &revocations,
            now_ns: 10,
        };
        let migrate = || {
            migrate_legacy_personal_rooms(&legacy, &directory, &binding, |bytes| Ok(bytes.to_vec()))
        };
        crate::persist::write_public_key(&paths.user_pubkey, &foreign.verifying_key().to_bytes())
            .unwrap();
        assert!(migrate().is_err());
        crate::persist::write_public_key(&paths.user_pubkey, &identity).unwrap();
        crate::persist::write_device_cert(&paths.device_cert, &cert(&foreign, 4)).unwrap();
        assert!(migrate().is_err());
        let mut forged = valid.clone();
        forged.signature[0] ^= 1;
        crate::persist::write_device_cert(&paths.device_cert, &forged).unwrap();
        assert!(migrate().is_err());
        crate::persist::write_device_cert(&paths.device_cert, &valid).unwrap();
        // Fleet corroboration can recover a missing default certificate, but
        // cannot substitute another root's key for the legacy root binding.
        std::fs::remove_file(&paths.device_cert).unwrap();
        assert!(migrate().is_err());
        crate::persist::write_device_cert(&paths.device_cert, &valid).unwrap();
        let file = open(&legacy, libc::O_RDONLY, 0).unwrap();
        let owner = file.metadata().unwrap().uid();
        if owner == 0 {
            // Native test runners may run as root. A root-owned source is
            // deliberately trusted, so give this artifact a foreign owner.
            std::os::unix::fs::chown(&legacy, Some(1), None).unwrap();
            assert!(migrate().is_err());
            std::os::unix::fs::chown(&legacy, Some(owner), None).unwrap();
        } else {
            assert!(trusted_file(&file, owner + 1).is_err());
        }
        let link = temp.path().join("linked-rooms.json");
        std::fs::hard_link(&legacy, &link).unwrap();
        assert!(migrate().is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::rename(&legacy, &link).unwrap();
        std::os::unix::fs::symlink(&link, &legacy).unwrap();
        assert!(migrate().is_err());
        std::fs::remove_file(&legacy).unwrap();
        std::fs::rename(&link, &legacy).unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(migrate().is_err());
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o640)).unwrap();
        std::fs::write(&legacy, vec![b' '; MAX_BYTES as usize + 1]).unwrap();
        assert!(migrate().is_err());
        std::fs::write(&legacy, bytes).unwrap();
        assert!(migrate().unwrap());
        assert_eq!(
            std::fs::read(directory.join("personal-rooms.json")).unwrap(),
            bytes
        );
        std::fs::write(directory.join("personal-rooms.json"), b"[]").unwrap();
        assert!(!migrate().unwrap());
        assert_eq!(
            std::fs::read(directory.join("personal-rooms.json")).unwrap(),
            b"[]"
        );
    }

    #[test]
    fn room_import_can_use_a_verified_account_fleet_but_not_a_revoked_device() {
        let temp = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let identity = root.verifying_key().to_bytes();
        let directory = person(temp.path(), &root);
        let paths = crate::IdentityPaths::under(temp.path());
        paths.ensure_directories().unwrap();
        crate::persist::write_public_key(&paths.user_pubkey, &identity).unwrap();
        let certificates = [cert(&root, 3)];
        let legacy = temp.path().join("personal-rooms.json");
        std::fs::write(&legacy, b"[]").unwrap();
        let mut entry = RevocationEntry {
            user_public_key: identity,
            revoked_device_public_key: certificates[0].device_public_key,
            revoked_at_ns: u64::MAX,
            reason: RevocationReason::Lost,
            signature: [0; 64],
        };
        sign_revocation_entry(&mut entry, &root).unwrap();
        let mut revoked = RevocationList::new();
        revoked.insert(entry).unwrap();
        let mut binding = LegacyRoomBinding {
            identity: &identity,
            paths: &paths,
            certificates: &certificates,
            revocations: &revoked,
            now_ns: 10,
        };
        let publish = |binding: &LegacyRoomBinding<'_>| {
            migrate_legacy_personal_rooms(&legacy, &directory, binding, |bytes| Ok(bytes.to_vec()))
        };
        assert!(
            publish(&binding).is_err(),
            "future-dated revocations are permanent"
        );
        assert!(!directory.join("personal-rooms.json").exists());
        let clear = RevocationList::new();
        binding.revocations = &clear;
        assert!(
            publish(&binding).unwrap(),
            "a default cert is not required when the account fleet corroborates the root"
        );
    }

    #[test]
    fn migration_partitions_people_by_verified_certificates_and_permanent_revocations() {
        let temp = tempfile::tempdir().unwrap();
        let alice = SigningKey::from_bytes(&[1; 32]);
        let bob = SigningKey::from_bytes(&[2; 32]);
        let a = cert(&alice, 3);
        let b = cert(&bob, 4);
        let missing = cert(&alice, 5);
        let revoked = cert(&alice, 6);
        let mut forged = cert(&alice, 7);
        forged.signature[0] ^= 1;
        let mut expired = cert(&alice, 8);
        expired.expires_at_ns = Some(10);
        sign_device_cert(&mut expired, &alice).unwrap();
        let mut future = cert(&alice, 9);
        future.issued_at_ns = 11;
        sign_device_cert(&mut future, &alice).unwrap();
        let mut host = cert(&alice, 10);
        host.capabilities = DeviceCapabilities::host();
        sign_device_cert(&mut host, &alice).unwrap();
        let certificates = [
            a.clone(),
            b.clone(),
            revoked.clone(),
            forged,
            expired,
            future,
            host,
        ];
        let legacy = temp.path().join("personal-device-peers.json");
        let hints: Vec<_> = certificates
            .iter()
            .chain([&missing, &a])
            .map(|cert| hex::encode(cert.device_public_key))
            .collect();
        std::fs::write(&legacy, serde_json::to_vec(&hints).unwrap()).unwrap();
        let mut revocations = RevocationList::new();
        for (root, device) in [
            (&alice, revoked.device_public_key),
            (&bob, a.device_public_key),
        ] {
            let mut entry = RevocationEntry {
                user_public_key: root.verifying_key().to_bytes(),
                revoked_device_public_key: device,
                revoked_at_ns: u64::MAX,
                reason: RevocationReason::Lost,
                signature: [0; 64],
            };
            sign_revocation_entry(&mut entry, root).unwrap();
            revocations.insert(entry).unwrap();
        }
        for (root, expected) in [(&alice, &a), (&bob, &b)] {
            let directory = person(temp.path(), root);
            assert!(
                migrate_legacy_personal_peers(
                    &legacy,
                    &directory,
                    &root.verifying_key().to_bytes(),
                    &certificates,
                    &revocations,
                    10
                )
                .unwrap()
            );
            assert_eq!(
                devices(&directory),
                [hex::encode(expected.device_public_key)]
            );
            assert_eq!(
                std::fs::metadata(directory.join("personal-device-peers.json"))
                    .unwrap()
                    .mode()
                    & 0o777,
                0o640
            );
            std::fs::write(directory.join("personal-device-peers.json"), b"[]").unwrap();
            assert!(
                !migrate_legacy_personal_peers(
                    &legacy,
                    &directory,
                    &root.verifying_key().to_bytes(),
                    &certificates,
                    &revocations,
                    10
                )
                .unwrap()
            );
            assert!(
                devices(&directory).is_empty(),
                "removed devices cannot return through the global list"
            );
        }
        assert!(legacy.exists(), "other accounts can migrate independently");
    }

    #[test]
    fn migration_without_corroborating_certificates_is_empty_and_unsafe_sources_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = SigningKey::from_bytes(&[1; 32]);
        let directory = person(temp.path(), &root);
        let identity = root.verifying_key().to_bytes();
        let legacy = temp.path().join("legacy.json");
        std::fs::write(
            &legacy,
            serde_json::to_vec(&[hex::encode(cert(&root, 3).device_public_key)]).unwrap(),
        )
        .unwrap();
        let revocations = RevocationList::new();
        let migrate =
            || migrate_legacy_personal_peers(&legacy, &directory, &identity, &[], &revocations, 10);
        let link = temp.path().join("linked.json");
        std::fs::hard_link(&legacy, &link).unwrap();
        assert!(migrate().is_err());
        std::fs::remove_file(&link).unwrap();
        std::fs::rename(&legacy, &link).unwrap();
        std::os::unix::fs::symlink(&link, &legacy).unwrap();
        assert!(migrate().is_err());
        std::fs::remove_file(&legacy).unwrap();
        std::fs::rename(&link, &legacy).unwrap();
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(migrate().is_err());
        std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(migrate().unwrap());
        assert!(devices(&directory).is_empty());
    }
}
