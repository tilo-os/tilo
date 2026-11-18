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

#[test]
fn vault_seals_round_trip_under_their_own_format_only() {
    {
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
    {
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
    {
        let rendered = format!("{:?}", key(0xab));
        assert!(!rendered.contains("abab"), "{rendered}");
        assert!(!rendered.contains("171, 171"), "{rendered}");
    }
    {
        let key = key(7);
        let first = FORMAT.seal(&key, b"same").unwrap();
        let second = FORMAT.seal(&key, b"same").unwrap();
        assert_ne!(first, second);
    }
}

#[test]
fn secret_service_keys_encode_canonically() {
    {
        let key = [0xa5; KEY_LEN];
        assert_eq!(decode_key(&encode_key(&key)).unwrap().expose_secret(), &key);
        assert!(decode_key("too-short").is_err());
        assert!(decode_key(&"z".repeat(KEY_LEN * 2)).is_err());
    }
    {
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
            "stored vault key has the wrong length"
        );
        assert_eq!(
            decode_key(&format!("{short}g")).unwrap_err(),
            "stored vault key is not hexadecimal"
        );
        // A multi-byte character of the right byte length is not hex either.
        let shorter = encoded.get(..KEY_LEN * 2 - 2).unwrap();
        assert_eq!(
            decode_key(&format!("{shorter}é")).unwrap_err(),
            "stored vault key is not hexadecimal"
        );
    }
}

#[test]
fn local_keys_are_created_once_and_never_replaced_or_minted_beside_a_vault() {
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("vault");
        let created = FORMAT
            .load_or_create_key(&dir, VaultBackend::LocalFile)
            .unwrap();
        assert_eq!(persisted_key(&dir).as_slice(), created.expose_secret());
        let reloaded = FORMAT
            .load_or_create_key(&dir, VaultBackend::LocalFile)
            .unwrap();
        assert_eq!(reloaded, created);
    }
    {
        for _ in 0..8 {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().to_path_buf();
            let hold = std::sync::Arc::new(std::sync::Barrier::new(8));
            // Spawn every member before joining: interleaving spawn/join
            // would deadlock the barrier.
            let mut workers = Vec::with_capacity(8);
            for _ in 0..8 {
                let dir = dir.clone();
                let hold = std::sync::Arc::clone(&hold);
                workers.push(std::thread::spawn(move || {
                    hold.wait();
                    FORMAT
                        .load_or_create_key(&dir, VaultBackend::LocalFile)
                        .unwrap()
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
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        tilo_files::native::private::create_atomic(&dir.join(KEY_FILE), &[0x11; KEY_LEN]).unwrap();
        let key = FORMAT
            .load_or_create_key(dir, VaultBackend::LocalFile)
            .unwrap();
        assert_eq!(key.expose_secret(), &[0x11; KEY_LEN]);
        assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), [0x11; KEY_LEN]);
    }
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let sealed = FORMAT.seal(&key(3), b"orphaned").unwrap();
        std::fs::write(dir.join(VAULT_FILE), &sealed).unwrap();
        assert!(
            FORMAT
                .load_or_create_key(dir, VaultBackend::LocalFile)
                .is_none()
        );
        assert!(!dir.join(KEY_FILE).exists());
        assert_eq!(std::fs::read(dir.join(VAULT_FILE)).unwrap(), sealed);
    }
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        tilo_files::native::private::create_atomic(&dir.join(KEY_FILE), b"short").unwrap();
        assert!(
            FORMAT
                .load_or_create_key(dir, VaultBackend::LocalFile)
                .is_none()
        );
        assert_eq!(std::fs::read(dir.join(KEY_FILE)).unwrap(), b"short");
    }
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        std::fs::create_dir(dir.join(KEY_FILE)).unwrap();
        assert!(
            FORMAT
                .load_or_create_key(dir, VaultBackend::LocalFile)
                .is_none()
        );
        assert!(dir.join(KEY_FILE).is_dir());
    }
}

#[cfg(unix)]
#[test]
fn local_keys_are_owner_only_and_non_private_keys_are_kept() {
    {
        use std::os::unix::fs::PermissionsExt;
        // A readable-but-not-private key exists and cannot be used, so it must
        // survive untouched alongside the vault it seals. (An owner-unreadable
        // mode cannot be exercised here: root reads it anyway. A key that cannot
        // be read at all is covered by the directory case below.)
        let mode = 0o644;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let key_path = dir.join(KEY_FILE);
        tilo_files::native::private::create_atomic(&key_path, &[0x22; KEY_LEN]).unwrap();
        let sealed = FORMAT.seal(&key(0x22), b"still mine").unwrap();
        std::fs::write(dir.join(VAULT_FILE), &sealed).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(mode)).unwrap();

        assert!(
            FORMAT
                .load_or_create_key(dir, VaultBackend::LocalFile)
                .is_none(),
            "mode {mode:o} key must not be used or replaced"
        );

        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read(&key_path).unwrap(), [0x22; KEY_LEN]);
        assert_eq!(std::fs::read(dir.join(VAULT_FILE)).unwrap(), sealed);
        let entries: Vec<_> = std::fs::read_dir(dir).unwrap().collect();
        assert_eq!(entries.len(), 2, "no replacement key was staged");
    }
    {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        FORMAT
            .load_or_create_key(root.path(), VaultBackend::LocalFile)
            .unwrap();
        let mode = std::fs::metadata(root.path().join(KEY_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // The local key is the raw key in a file beside the vault: whoever can
    // read the directory can open the vault. Its label must not promise the
    // protection the keyring gives.
    {
        let root = tempfile::tempdir().unwrap();
        let key = FORMAT
            .load_or_create_key(root.path(), VaultBackend::LocalFile)
            .unwrap();
        let sealed = FORMAT.seal(&key, b"every password").unwrap();
        let from_file = key_from_bytes(&persisted_key(root.path())).unwrap();
        assert_eq!(
            FORMAT.open(&from_file, &sealed).unwrap().as_slice(),
            b"every password",
            "the key file alone opens the vault"
        );
        let label = VaultBackend::LocalFile.label().to_lowercase();
        assert!(label.contains("file"), "{label:?} must name the key file");
        assert_ne!(
            VaultBackend::LocalFile.label(),
            VaultBackend::System.label()
        );
        for overclaim in ["encrypt", "protect", "secure", "sealed"] {
            assert!(
                !label.contains(overclaim),
                "{label:?} claims {overclaim:?} for a key stored in the clear"
            );
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_dpapi_keys_converge_and_bind_to_their_format() {
    {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_path_buf();
        let hold = std::sync::Arc::new(std::sync::Barrier::new(4));
        let mut workers = Vec::with_capacity(4);
        for _ in 0..4 {
            let dir = dir.clone();
            let hold = std::sync::Arc::clone(&hold);
            workers.push(std::thread::spawn(move || {
                hold.wait();
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
        assert!(tilo_files::native::private::read(&dir.join("vault.key.dpapi")).is_ok());
    }
    {
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
}

/// A key kept as a file, as a machine with no keyring keeps it.
const AS_A_FILE: VaultBackend = VaultBackend::LocalFile;

/// A vault under a folder whose keys are kept apart: one sealed blob in
/// `stores/<who>/vault`, and the key it was sealed under.
fn sealed_vault(stores: &Path, who: &str) -> (std::path::PathBuf, VaultKey) {
    let vault = stores.join(who).join("vault");
    let key = FORMAT
        .load_or_create_key(&vault, AS_A_FILE)
        .expect("a key is made");
    std::fs::create_dir_all(&vault).unwrap();
    let sealed = FORMAT.seal(&key, b"sealed").unwrap();
    std::fs::write(vault.join(VAULT_FILE), sealed).unwrap();
    (vault, key)
}

#[test]
fn a_key_kept_apart_never_lies_beside_its_vault_and_a_copy_of_the_vault_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let (stores, keys) = (dir.path().join("stores"), dir.path().join("keys"));
    keep_keys_apart(&stores, &keys);
    let (vault, key) = sealed_vault(&stores, "ada");
    assert!(!vault.join(KEY_FILE).exists(), "no key beside the vault");
    let apart = keys.join("ada").join("vault");
    assert_eq!(persisted_key(&apart).as_slice(), key.expose_secret());
    let again = FORMAT.load_or_create_key(&vault, AS_A_FILE);
    assert!(
        again.is_some_and(|again| again.eq(&key)),
        "the same key every time"
    );
    let read = FORMAT.read_existing_key(&vault, AS_A_FILE);
    assert!(read.is_some_and(|read| read.eq(&key)), "its key is read");

    // A copy of the vaults' folder, with no keys: nothing reads its key,
    // and nothing mints one for a vault that is sealed already.
    let copy = dir.path().join("copy").join("ada").join("vault");
    std::fs::create_dir_all(&copy).unwrap();
    std::fs::copy(vault.join(VAULT_FILE), copy.join(VAULT_FILE)).unwrap();
    assert!(FORMAT.read_existing_key(&copy, AS_A_FILE).is_none());
    assert!(FORMAT.load_or_create_key(&copy, AS_A_FILE).is_none());
    assert!(!copy.join(KEY_FILE).exists());
    // Nor does the folder the keys are kept under open without its vaults'.
    keep_keys_apart(&dir.path().join("copy"), &dir.path().join("no-keys"));
    assert!(FORMAT.load_or_create_key(&copy, AS_A_FILE).is_none());
    assert!(!dir.path().join("no-keys").exists(), "and no key is made");
}

#[test]
fn a_key_left_beside_its_vault_moves_apart_once_it_was_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let (stores, keys) = (dir.path().join("stores"), dir.path().join("keys"));
    // Sealed before its keys were kept apart: the key lies beside it.
    let (vault, key) = sealed_vault(&stores, "bo");
    assert_eq!(persisted_key(&vault).as_slice(), key.expose_secret());

    keep_keys_apart(&stores, &keys);
    // Reading moves nothing, and still finds the key.
    let read = FORMAT.read_existing_key(&vault, AS_A_FILE);
    assert!(read.is_some_and(|read| read.eq(&key)), "its key is read");
    assert!(vault.join(KEY_FILE).exists());

    let moved = FORMAT
        .load_or_create_key(&vault, AS_A_FILE)
        .expect("the vault keeps its key");
    assert!(moved.eq(&key), "the very key it was sealed under");
    assert!(
        !vault.join(KEY_FILE).exists(),
        "which lies beside it no more"
    );
    let apart = keys.join("bo").join("vault");
    assert_eq!(persisted_key(&apart).as_slice(), key.expose_secret());
    let blob = std::fs::read(vault.join(VAULT_FILE)).unwrap();
    assert_eq!(FORMAT.open(&moved, &blob).unwrap().as_slice(), b"sealed");
    // A second vault made from now on has its key apart from the start.
    let (other, _) = sealed_vault(&stores, "cy");
    assert!(!other.join(KEY_FILE).exists());
}
