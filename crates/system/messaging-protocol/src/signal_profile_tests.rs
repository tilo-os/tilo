use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use tilo_messaging::SendError;

struct Authority(AtomicBool);
impl AccountAuthority for Authority {
    fn check(&self) -> Result<(), SendError> {
        if self.0.load(Ordering::Acquire) {
            Ok(())
        } else {
            Err(SendError::Revoked)
        }
    }
}
fn authority() -> Arc<Authority> {
    Arc::new(Authority(AtomicBool::new(true)))
}
fn key() -> VaultKey {
    VaultKey::from_slice(&[7; 32]).unwrap()
}
fn owner() -> Owner {
    Owner {
        principal: "11".repeat(32),
        device: "fixture-device".into(),
        service: Service::Signal,
        account: "fixture-account".into(),
        linking_generation: 1,
    }
}
fn root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("profile");
    (temp, root)
}

#[test]
fn private_profile_visit_stops_on_original_retirement_or_storage_failure() {
    let (_temp, root) = root();
    let source = authority();
    let mut store =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), source.clone()).unwrap();
    let mut image = store.begin_use().unwrap();
    image
        .put("a/first".into(), Zeroizing::new(vec![1]))
        .unwrap();
    image
        .put("b/second".into(), Zeroizing::new(vec![2]))
        .unwrap();
    let mut count = 0;
    assert_eq!(
        image.visit_files(|_, _| {
            count += 1;
            Err(ProfileError::Unavailable)
        }),
        Err(ProfileError::Unavailable)
    );
    assert_eq!(count, 1);
    count = 0;
    assert_eq!(
        image.visit_files(|path, bytes| {
            count += 1;
            if path != "a/first" || bytes != [1] {
                return Err(ProfileError::InvalidImage);
            }
            source.0.store(false, Ordering::Release);
            Ok(())
        }),
        Err(ProfileError::Revoked)
    );
    assert_eq!(count, 1);
    store.checkpoint(image).unwrap();
}

#[test]
fn sealed_profile_round_trip_and_original_authority_retirement() {
    let (_temp, root) = root();
    let source = authority();
    let mut store =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), source.clone()).unwrap();
    let mut image = store.begin_use().unwrap();
    image
        .put(
            "data/account.db".into(),
            Zeroizing::new(b"fixture-ratchet-state".to_vec()),
        )
        .unwrap();
    source.0.store(false, Ordering::Release);
    assert_eq!(image.file("data/account.db"), Err(ProfileError::Revoked));
    // Retirement permits seal-only cleanup, never plaintext access or revival.
    store.checkpoint(image).unwrap();
    assert!(matches!(store.begin_use(), Err(ProfileError::Revoked)));
    drop(store);
    let sealed = std::fs::read(root.join("profile.enc")).unwrap();
    assert!(
        !sealed
            .windows(b"fixture-ratchet-state".len())
            .any(|part| part == b"fixture-ratchet-state")
    );
    assert!(
        !sealed
            .windows(b"account.db".len())
            .any(|part| part == b"account.db")
    );
    let mut reopened =
        EncryptedSignalProfile::open_with_key(&root, owner(), key(), authority()).unwrap();
    let image = reopened.begin_use().unwrap();
    assert_eq!(
        image.file("data/account.db").unwrap(),
        Some(b"fixture-ratchet-state".as_slice())
    );
    reopened.checkpoint(image).unwrap();
}

#[test]
fn interrupted_use_never_restores_old_crypto_state_or_resets() {
    let (_temp, root) = root();
    let mut store =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).unwrap();
    let mut image = store.begin_use().unwrap();
    image
        .put("ratchet".into(), Zeroizing::new(b"advanced-state".to_vec()))
        .unwrap();
    assert!(matches!(store.begin_use(), Err(ProfileError::Interrupted)));
    drop(image);
    drop(store);
    let before = std::fs::read(root.join("profile.enc")).unwrap();
    assert!(matches!(
        EncryptedSignalProfile::open_with_key(&root, owner(), key(), authority()),
        Err(ProfileError::Interrupted)
    ));
    assert!(EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).is_err());
    assert_eq!(std::fs::read(root.join("profile.enc")).unwrap(), before);
    assert!(root.join("active.enc").exists());
}

#[test]
fn owner_domains_and_key_corruption_refuse_without_overwrite() {
    let (_temp, root) = root();
    drop(EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).unwrap());
    let before = std::fs::read(root.join("profile.enc")).unwrap();
    for field in 0..5 {
        let mut other = owner();
        match field {
            0 => other.principal = "22".repeat(32),
            1 => other.device = "other-device".into(),
            2 => other.account = "other-account".into(),
            3 => other.linking_generation = 2,
            _ => other.service = Service::Whatsapp,
        }
        assert!(EncryptedSignalProfile::open_with_key(&root, other, key(), authority()).is_err());
    }
    assert!(
        EncryptedSignalProfile::open_with_key(
            &root,
            owner(),
            VaultKey::from_slice(&[9; 32]).unwrap(),
            authority()
        )
        .is_err()
    );
    assert_eq!(std::fs::read(root.join("profile.enc")).unwrap(), before);
    tilo_files::native::private::write_atomic(&root.join("profile.enc"), b"corrupt").unwrap();
    assert!(EncryptedSignalProfile::open_with_key(&root, owner(), key(), authority()).is_err());
    assert_eq!(std::fs::read(root.join("profile.enc")).unwrap(), b"corrupt");
}

#[test]
fn exclusive_lease_foreign_image_and_tampered_marker_refuse() {
    let (_temp, root) = root();
    let (_other_temp, other_root) = self::root();
    let mut first =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).unwrap();
    assert!(matches!(
        EncryptedSignalProfile::open_with_key(&root, owner(), key(), authority()),
        Err(ProfileError::Busy)
    ));
    let mut other =
        EncryptedSignalProfile::create_with_key(&other_root, owner(), key(), authority()).unwrap();
    let foreign = other.begin_use().unwrap();
    let image = first.begin_use().unwrap();
    assert!(matches!(
        first.checkpoint(foreign),
        Err(ProfileError::Interrupted)
    ));
    let before = std::fs::read(root.join("profile.enc")).unwrap();
    tilo_files::native::private::write_atomic(&root.join("active.enc"), b"tampered").unwrap();
    assert!(first.checkpoint(image).is_err());
    assert_eq!(std::fs::read(root.join("profile.enc")).unwrap(), before);
    assert!(root.join("active.enc").exists());
}

#[test]
fn retired_authority_refuses_before_storage_and_paths_are_bounded() {
    let (_temp, root) = root();
    let source = authority();
    source.0.store(false, Ordering::Release);
    assert!(matches!(
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), source),
        Err(ProfileError::Revoked)
    ));
    assert!(!root.exists());
    let mut store =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).unwrap();
    let mut image = store.begin_use().unwrap();
    for path in [
        "/absolute",
        "../escape",
        "a/../b",
        "a//b",
        "a\\b",
        "a\nb",
        "./a",
        "",
    ] {
        assert_eq!(
            image.put(path.into(), Zeroizing::new(vec![1])),
            Err(ProfileError::InvalidImage)
        );
    }
    image
        .put("data/state".into(), Zeroizing::new(vec![1]))
        .unwrap();
    assert!(image.remove("data/state").unwrap());
    assert!(!image.remove("data/state").unwrap());
    store.checkpoint(image).unwrap();
}

#[test]
fn stopped_profile_collection_replaces_deleted_files_and_seals_after_revocation() {
    let (_temp, root) = root();
    let source = authority();
    let mut store =
        EncryptedSignalProfile::create_with_key(&root, owner(), key(), source.clone()).unwrap();
    let mut image = store.begin_use().unwrap();
    image.put("old".into(), Zeroizing::new(vec![1])).unwrap();
    source.0.store(false, Ordering::Release);
    store
        .checkpoint_files(image, [("new".into(), Zeroizing::new(vec![2]))])
        .unwrap();
    assert!(!root.join("active.enc").exists());
    assert!(matches!(store.begin_use(), Err(ProfileError::Revoked)));
    drop(store);
    let mut store =
        EncryptedSignalProfile::open_with_key(&root, owner(), key(), authority()).unwrap();
    let image = store.begin_use().unwrap();
    assert_eq!(image.file("old").unwrap(), None);
    assert_eq!(image.file("new").unwrap(), Some([2].as_slice()));
    store.checkpoint(image).unwrap();
}

#[test]
fn invalid_stopped_collection_preserves_interrupted_marker_and_prior_ciphertext() {
    for paths in [["same", "same"], ["../outside", "new"]] {
        let (_temp, root) = root();
        let mut store =
            EncryptedSignalProfile::create_with_key(&root, owner(), key(), authority()).unwrap();
        let before = std::fs::read(root.join("profile.enc")).unwrap();
        let image = store.begin_use().unwrap();
        assert_eq!(
            store.checkpoint_files(
                image,
                paths.map(|path| (path.into(), Zeroizing::new(vec![9])))
            ),
            Err(ProfileError::InvalidImage)
        );
        assert!(root.join("active.enc").exists());
        assert_eq!(std::fs::read(root.join("profile.enc")).unwrap(), before);
        assert!(matches!(store.begin_use(), Err(ProfileError::Interrupted)));
    }
}
