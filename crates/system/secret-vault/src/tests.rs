use super::*;

const FORMAT: VaultFormat = VaultFormat {
    name: "test",
    magic: b"TSTV",
    secret_service_label: "Tilo secret-vault test",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "secret-vault-test")],
    dpapi_entropy: b"Tilo secret-vault test key v1",
};

const OTHER: VaultFormat = VaultFormat {
    name: "other",
    magic: b"OTHR",
    ..FORMAT
};

fn persisted_key(dir: &Path) -> Zeroizing<Vec<u8>> {
    read_key_bytes(&dir.join(KEY_FILE)).unwrap()
}

fn key(byte: u8) -> VaultKey {
    VaultKey::from_bytes([byte; KEY_LEN])
}

/// `FORMAT.seal(&[7; KEY_LEN], b"legacy vault bytes")` in the layout written
/// before `tilo-seal` envelopes, produced by that implementation's sealing
/// code with the nonce fixed to `[0x5a; 24]`.
const LEGACY_BLOB: &str = concat!(
    "54535456015a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5ac48e22",
    "5b2560c03ed2200c7b1aaad80f16f98f05b43a3c8d48100b282469fdd928d5",
);

#[test]
fn seal_open_round_trips_and_rejects_tampering() {
    let key = key(7);
    let plaintext = b"secret vault bytes";
    let blob = FORMAT.seal(&key, plaintext).expect("seal");
    assert!(!blob.windows(plaintext.len()).any(|w| w == plaintext));
    assert!(tilo_seal::envelope::is_envelope(&blob));
    assert_eq!(FORMAT.open(&key, &blob).unwrap().as_slice(), plaintext);
    assert!(FORMAT.open(&self::key(9), &blob).is_none());
    let mut tampered = blob.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;
    assert!(FORMAT.open(&key, &tampered).is_none());
    // Truncation and an unknown envelope version never yield bytes.
    assert!(FORMAT.open(&key, &blob[..blob.len() - 17]).is_none());
    let mut bumped = blob;
    bumped[5] = bumped[5].wrapping_add(1);
    assert!(FORMAT.open(&key, &bumped).is_none());
}

#[test]
fn a_vault_never_opens_under_another_format() {
    let key = key(7);
    let blob = FORMAT.seal(&key, b"format-bound").unwrap();
    assert!(OTHER.open(&key, &blob).is_none());
    // The name is part of the identity too, not only the magic.
    let renamed = VaultFormat {
        name: "renamed",
        ..FORMAT
    };
    assert!(renamed.open(&key, &blob).is_none());
    assert!(FORMAT.open(&key, &blob).is_some());
}

#[test]
fn legacy_vaults_open_only_under_their_own_magic() {
    let key = key(7);
    let blob = hex::decode(LEGACY_BLOB).unwrap();
    assert_eq!(&blob[..4], FORMAT.magic.as_slice());
    assert_eq!(blob[4], LEGACY_SEAL_FORMAT);
    assert_eq!(
        FORMAT.open(&key, &blob).unwrap().as_slice(),
        b"legacy vault bytes"
    );
    assert!(OTHER.open(&key, &blob).is_none());
    assert!(FORMAT.open(&self::key(9), &blob).is_none());
    // Truncation and an unknown format byte never reach the AEAD.
    assert!(
        FORMAT
            .open(&key, &blob[..4 + 1 + LEGACY_NONCE_LEN - 1])
            .is_none()
    );
    let mut bumped = blob.clone();
    bumped[4] = LEGACY_SEAL_FORMAT + 1;
    assert!(FORMAT.open(&key, &bumped).is_none());
    // Re-sealing moves the vault to the current envelope.
    let resealed = FORMAT
        .seal(&key, &FORMAT.open(&key, &blob).unwrap())
        .unwrap();
    assert!(tilo_seal::envelope::is_envelope(&resealed));
    assert_eq!(
        FORMAT.open(&key, &resealed).unwrap().as_slice(),
        b"legacy vault bytes"
    );
}

#[test]
fn vault_key_debug_never_shows_key_bytes() {
    let rendered = format!("{:?}", key(0xab));
    assert!(!rendered.contains("abab"), "{rendered}");
    assert!(!rendered.contains("171, 171"), "{rendered}");
}

#[test]
fn fresh_seals_use_distinct_nonces() {
    let key = key(7);
    let first = FORMAT.seal(&key, b"same").unwrap();
    let second = FORMAT.seal(&key, b"same").unwrap();
    assert_ne!(first, second);
}

#[test]
fn secret_service_key_encoding_round_trips_and_rejects_bad_material() {
    let key = [0xa5; KEY_LEN];
    assert_eq!(decode_key(&encode_key(&key)).unwrap().expose_secret(), &key);
    assert!(decode_key("too-short").is_err());
    assert!(decode_key(&"z".repeat(KEY_LEN * 2)).is_err());
}

#[test]
fn secret_service_key_encoding_is_lowercase_and_decoding_accepts_either_case() {
    let mut key = [0u8; KEY_LEN];
    for (byte, value) in key.iter_mut().zip(0xe0_u8..=0xff) {
        *byte = value;
    }
    let encoded = encode_key(&key);
    assert_eq!(
        encoded,
        "e0e1e2e3e4e5e6e7e8e9eaebecedeeeff0f1f2f3f4f5f6f7f8f9fafbfcfdfeff"
    );
    assert_eq!(
        decode_key(&encoded.to_ascii_uppercase())
            .unwrap()
            .expose_secret(),
        &key
    );
    let short = encoded.get(..KEY_LEN * 2 - 1).unwrap();
    assert_eq!(
        decode_key(short).unwrap_err(),
        "Secret Service key has the wrong length"
    );
    assert_eq!(
        decode_key(&format!("{short}g")).unwrap_err(),
        "Secret Service key is not hexadecimal"
    );
    // A multi-byte character of the right byte length is not hex either.
    let shorter = encoded.get(..KEY_LEN * 2 - 2).unwrap();
    assert_eq!(
        decode_key(&format!("{shorter}é")).unwrap_err(),
        "Secret Service key is not hexadecimal"
    );
}

#[test]
fn secret_service_arguments_carry_the_format_identity() {
    assert_eq!(
        FORMAT.secret_service_args("lookup", None),
        [
            "lookup",
            "application",
            "Tilo",
            "purpose",
            "secret-vault-test"
        ]
    );
    assert_eq!(
        FORMAT.secret_service_args("store", Some(FORMAT.secret_service_label)),
        [
            "store",
            "--label=Tilo secret-vault test",
            "application",
            "Tilo",
            "purpose",
            "secret-vault-test"
        ]
    );
}

#[test]
fn local_key_is_created_once_and_reloaded() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("vault");
    let created = FORMAT.load_or_create_key(&dir, Custody::LocalFile).unwrap();
    assert_eq!(persisted_key(&dir).as_slice(), created.expose_secret());
    let reloaded = FORMAT.load_or_create_key(&dir, Custody::LocalFile).unwrap();
    assert_eq!(reloaded, created);
}

#[test]
fn simultaneous_creators_converge_on_the_one_persisted_key() {
    for _ in 0..8 {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(8));
        // Spawn every participant before joining: interleaving spawn/join
        // would deadlock the barrier.
        let mut workers = Vec::with_capacity(8);
        for _ in 0..8 {
            let dir = dir.clone();
            let gate = std::sync::Arc::clone(&gate);
            workers.push(std::thread::spawn(move || {
                gate.wait();
                FORMAT.load_or_create_key(&dir, Custody::LocalFile).unwrap()
            }));
        }
        let keys: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        let persisted = persisted_key(&dir);
        assert!(
            keys.iter()
                .all(|key| key.expose_secret().as_slice() == persisted.as_slice())
        );
        // Losing creators leave no temporary files behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }
}

#[test]
fn an_existing_key_is_never_replaced_by_a_creator() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    private_fs::create_atomic(&dir.join(KEY_FILE), &[0x11; KEY_LEN]).unwrap();
    let key = FORMAT.load_or_create_key(dir, Custody::LocalFile).unwrap();
    assert_eq!(key.expose_secret(), &[0x11; KEY_LEN]);
    assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), [0x11; KEY_LEN]);
}

#[test]
fn missing_key_beside_an_existing_vault_is_never_minted() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let sealed = FORMAT.seal(&key(3), b"orphaned").unwrap();
    std::fs::write(dir.join(VAULT_FILE), &sealed).unwrap();
    assert!(FORMAT.load_or_create_key(dir, Custody::LocalFile).is_none());
    assert!(!dir.join(KEY_FILE).exists());
    assert_eq!(std::fs::read(dir.join(VAULT_FILE)).unwrap(), sealed);
}

#[test]
fn malformed_key_is_never_overwritten() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    private_fs::create_atomic(&dir.join(KEY_FILE), b"short").unwrap();
    assert!(FORMAT.load_or_create_key(dir, Custody::LocalFile).is_none());
    assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), b"short");
}

#[cfg(unix)]
#[test]
fn a_key_that_is_not_private_is_never_replaced() {
    use std::os::unix::fs::PermissionsExt;
    // A readable-but-not-private key exists and cannot be used, so it must
    // survive untouched alongside the vault it seals. (An owner-unreadable
    // mode cannot be exercised here: root reads it anyway. A key that cannot
    // be read at all is covered by the directory case below.)
    let mode = 0o644;
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let key_path = dir.join(KEY_FILE);
    private_fs::create_atomic(&key_path, &[0x22; KEY_LEN]).unwrap();
    let sealed = FORMAT.seal(&key(0x22), b"still mine").unwrap();
    std::fs::write(dir.join(VAULT_FILE), &sealed).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(mode)).unwrap();

    assert!(
        FORMAT.load_or_create_key(dir, Custody::LocalFile).is_none(),
        "mode {mode:o} key must not be used or replaced"
    );

    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(std::fs::read(&key_path).unwrap(), [0x22; KEY_LEN]);
    assert_eq!(std::fs::read(dir.join(VAULT_FILE)).unwrap(), sealed);
    let entries: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
    assert_eq!(entries.len(), 2, "no replacement key was staged");
}

#[test]
fn a_key_path_that_is_not_a_file_is_never_replaced() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    std::fs::create_dir(dir.join(KEY_FILE)).unwrap();
    assert!(FORMAT.load_or_create_key(dir, Custody::LocalFile).is_none());
    assert!(dir.join(KEY_FILE).is_dir());
}

#[cfg(unix)]
#[test]
fn created_key_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    FORMAT
        .load_or_create_key(root.path(), Custody::LocalFile)
        .unwrap();
    let mode = std::fs::metadata(root.path().join(KEY_FILE))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[cfg(windows)]
#[test]
fn windows_dpapi_concurrent_creation_returns_one_persisted_key() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().to_path_buf();
    let gate = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut workers = Vec::with_capacity(4);
    for _ in 0..4 {
        let dir = dir.clone();
        let gate = std::sync::Arc::clone(&gate);
        workers.push(std::thread::spawn(move || {
            gate.wait();
            windows::load_or_create_key(&FORMAT, &dir).unwrap()
        }));
    }
    let keys: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    let persisted = windows::load_or_create_key(&FORMAT, &dir).unwrap();
    assert!(
        keys.iter()
            .all(|key| key.as_slice() == persisted.as_slice())
    );
    assert!(!dir.join(KEY_FILE).exists());
    assert!(private_fs::read(&dir.join("vault.key.dpapi")).is_ok());
}

#[cfg(windows)]
#[test]
fn windows_dpapi_entropy_binds_the_protected_key_to_its_format() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path();
    let key = windows::load_or_create_key(&FORMAT, dir).unwrap();
    let other = VaultFormat {
        dpapi_entropy: b"a different vault",
        ..FORMAT
    };
    assert!(windows::load_or_create_key(&other, dir).is_err());
    assert_eq!(
        windows::load_or_create_key(&FORMAT, dir)
            .unwrap()
            .as_slice(),
        key.as_slice()
    );
}
