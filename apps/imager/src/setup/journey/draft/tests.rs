use super::*;
use std::{
    fs,
    os::unix::fs::{PermissionsExt as _, symlink},
};
use tilo_identity_host::provisioning::IdentityStorage;

fn choices() -> PublicChoices {
    PublicChoices {
        destination: Some(DestinationChoice::Another),
        migrate: Some(true),
        categories: [true, false, true],
        browser_domains: [true, false],
        excluded_flatpak_apps: std::collections::BTreeSet::default(),
        device_label: "My laptop".into(),
        purpose: install_profile_model::default_purpose(),
    }
}
fn namespace(root: &Path) -> PathBuf {
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.join("preparations");
    fs::create_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}
#[test]
fn closed_preparation_reopens_exact_identity_photo_and_public_choices_without_secrets() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = namespace(&root);
    let store = DraftStore::open(&path).unwrap();
    assert!(matches!(store.review().unwrap(), DraftStatus::Missing));
    let reserved = store.reserve(&choices(), IdentityKind::New).unwrap();
    assert!(matches!(store.review().unwrap(), DraftStatus::Pending(_)));
    let custody = reserved.identity_directory();
    let prepared = Arc::new(
        PreparedIdentity::create(&custody, "Ada", [1, 2, 3], IdentityStorage::Files).unwrap(),
    );
    let photo = runtime_identity::ProfilePhoto::new(1, 1, vec![12, 34, 56, 255]).unwrap();
    prepared.update_photo(Some(photo.clone())).unwrap();
    prepared.update_name("Quinten").unwrap();
    let before = runtime_identity::load_from_path(custody.join("primary.json"))
        .unwrap()
        .sign(b"restart-proof")
        .unwrap();
    store.acknowledge(&reserved, &prepared, None).unwrap();
    let changed = PublicChoices {
        device_label: "Another device".into(),
        purpose: install_profile_model::default_purpose(),
        ..choices()
    };
    store.update_choices_for(&prepared, &changed).unwrap();
    drop(prepared);
    drop(reserved);
    drop(store);
    let reopened = DraftStore::open(&path).unwrap();
    let DraftStatus::Ready(ready) = reopened.review().unwrap() else {
        panic!("acknowledged custody missing");
    };
    assert_eq!(ready.kind(), IdentityKind::New);
    assert_eq!(ready.choices(), &changed);
    let prepared = ready.resume().unwrap();
    assert_eq!(prepared.metadata().unwrap().display_name, "Quinten");
    assert_eq!(prepared.metadata().unwrap().profile_photo, Some(photo));
    let after = runtime_identity::load_from_path(custody.join("primary.json"))
        .unwrap()
        .sign(b"restart-proof")
        .unwrap();
    assert_eq!(before, after);
    let draft = String::from_utf8(fs::read(path.join(INDEX)).unwrap()).unwrap();
    for forbidden in [
        "password",
        "avatar_path",
        "secret_bytes",
        "source_file",
        "grant",
        "private_key",
    ] {
        assert!(!draft.contains(forbidden));
    }
    reopened.retire().unwrap();
    assert!(matches!(reopened.review().unwrap(), DraftStatus::Missing));
    assert!(custody.join("primary.json").is_file());
    assert_ne!(
        reopened
            .reserve(&choices(), IdentityKind::New)
            .unwrap()
            .operation_directory(),
        custody.parent().unwrap()
    );
}
#[test]
fn pending_crash_and_changed_or_malformed_pointer_never_mint_replacement_or_repair() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = namespace(&root);
    let store = DraftStore::open(&path).unwrap();
    let reserved = store.reserve(&choices(), IdentityKind::New).unwrap();
    let prepared = Arc::new(
        PreparedIdentity::create(
            &reserved.identity_directory(),
            "Ada",
            [1, 2, 3],
            IdentityStorage::Files,
        )
        .unwrap(),
    );
    let identity_before = fs::read(reserved.identity_directory().join("primary.json")).unwrap();
    drop(store);
    let reopened = DraftStore::open(&path).unwrap();
    let DraftStatus::Pending(pending) = reopened.review().unwrap() else {
        panic!("pending reservation missing");
    };
    assert_eq!(pending.identity_directory(), reserved.identity_directory());
    assert_eq!(pending.kind(), IdentityKind::New);
    assert!(reopened.reserve(&choices(), IdentityKind::New).is_err());
    drop(prepared);
    let recovered = pending.recover_created_new().unwrap();
    assert!(matches!(reopened.review().unwrap(), DraftStatus::Ready(_)));
    assert_eq!(
        recovered.public_key(),
        runtime_identity::load_metadata_from_path(
            &reserved.identity_directory().join("primary.json")
        )
        .unwrap()
        .group_public_key
    );
    let index = path.join(INDEX);
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    value["preparation"]["root_key"] = serde_json::json!(vec![0u8; 32]);
    fs::write(&index, serde_json::to_vec(&value).unwrap()).unwrap();
    let tampered = fs::read(&index).unwrap();
    let DraftStatus::Ready(ready) = reopened.review().unwrap() else {
        panic!("typed draft missing");
    };
    assert!(ready.resume().is_err());
    assert_eq!(fs::read(&index).unwrap(), tampered);
    value["unexpected_password"] = serde_json::json!("not-permitted");
    fs::write(&index, serde_json::to_vec(&value).unwrap()).unwrap();
    let malformed = fs::read(&index).unwrap();
    assert!(reopened.review().is_err());
    assert!(reopened.retire().is_err());
    assert_eq!(fs::read(&index).unwrap(), malformed);
    fs::remove_file(&index).unwrap();
    symlink(reserved.identity_directory().join("primary.json"), &index).unwrap();
    assert!(reopened.review().is_err());
    assert_eq!(
        fs::read(reserved.identity_directory().join("primary.json")).unwrap(),
        identity_before
    );
    let alias = root.join("alias");
    symlink(&path, &alias).unwrap();
    assert!(DraftStore::open(&alias).is_err());
}

#[test]
fn authenticated_backup_retains_original_kind_and_outside_namespace_is_never_adopted() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = namespace(&root);
    let origin = Arc::new(
        PreparedIdentity::create(
            &root.join("origin"),
            "Original",
            [8, 9, 10],
            IdentityStorage::Files,
        )
        .unwrap(),
    );
    let store = DraftStore::open(&path).unwrap();
    assert!(
        store
            .record_prepared(&choices(), IdentityKind::New, &origin)
            .is_err()
    );
    assert!(matches!(store.review().unwrap(), DraftStatus::Missing));
    let backup = root.join("identity.sealed");
    let password = "several unrelated installer words";
    origin
        .export(&backup, zeroize::Zeroizing::new(password.into()))
        .unwrap();
    let reserved = store.reserve(&choices(), IdentityKind::Backup).unwrap();
    assert!(reserved.recover_created_new().is_err());
    let restored = Arc::new(
        PreparedIdentity::restore_backup(
            &reserved.identity_directory(),
            &backup,
            zeroize::Zeroizing::new(password.into()),
        )
        .unwrap(),
    );
    store.acknowledge(&reserved, &restored, None).unwrap();
    store
        .record_prepared(&choices(), IdentityKind::Backup, &restored)
        .unwrap();
    drop(restored);
    let DraftStatus::Ready(ready) = store.review().unwrap() else {
        panic!("restored backup missing");
    };
    assert_eq!(ready.kind(), IdentityKind::Backup);
    assert_eq!(ready.resume().unwrap().public_key(), origin.public_key());
    assert!(backup.is_file());
    assert!(
        !String::from_utf8(fs::read(path.join(INDEX)).unwrap())
            .unwrap()
            .contains(password)
    );
    store.retire().unwrap();
    assert!(ready.resume().is_err());
    assert!(reserved.identity_directory().join("primary.json").is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn stale_same_operation_account_checkpoint_cannot_replace_newer_selected_generation() {
    use super::super::accounts::checkpoint::Receipt;
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store = DraftStore::open(&namespace(&root)).unwrap();
    let pending = store.reserve(&choices(), IdentityKind::New).unwrap();
    let prepared = Arc::new(
        PreparedIdentity::create(
            &pending.identity_directory(),
            "Ada",
            [1, 2, 3],
            IdentityStorage::Files,
        )
        .unwrap(),
    );
    store.acknowledge(&pending, &prepared, None).unwrap();
    let DraftStatus::Ready(initial) = store.review().unwrap() else {
        panic!("missing current draft");
    };
    let first: Receipt = serde_json::from_value(serde_json::json!({
        "schema": 1,
        "filename": format!("{}.sealed", uuid::Uuid::new_v4()),
        "sealed_bytes": 256,
        "ciphertext_sha256": vec![1_u8; 32],
        "binding": pending.pointer.operation,
        "group": prepared.public_key(),
        "accounts": 1,
    }))
    .unwrap();
    store
        .record_account_checkpoint(&first, initial.account_checkpoint())
        .unwrap();
    let DraftStatus::Ready(before_update) = store.review().unwrap() else {
        panic!("missing account checkpoint");
    };
    assert_eq!(before_update.account_checkpoint(), Some(&first));
    let mut fields = serde_json::to_value(&first).unwrap();
    fields["filename"] = serde_json::json!(format!("{}.sealed", uuid::Uuid::new_v4()));
    fields["ciphertext_sha256"] = serde_json::json!(vec![2_u8; 32]);
    fields["accounts"] = serde_json::json!(0);
    let newer: Receipt = serde_json::from_value(fields).unwrap();
    store
        .record_account_checkpoint(&newer, before_update.account_checkpoint())
        .unwrap();
    let retained = fs::read(store.root.join(INDEX)).unwrap();
    assert_eq!(
        before_update.resume().err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    let invoked = std::cell::Cell::new(false);
    assert_eq!(
        before_update
            .with_current(|| {
                invoked.set(true);
                Ok(())
            })
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!invoked.get());
    let mut fields = serde_json::to_value(&first).unwrap();
    fields["filename"] = serde_json::json!(format!("{}.sealed", uuid::Uuid::new_v4()));
    fields["ciphertext_sha256"] = serde_json::json!(vec![3_u8; 32]);
    let stale: Receipt = serde_json::from_value(fields).unwrap();
    assert!(
        store
            .record_account_checkpoint(&stale, before_update.account_checkpoint())
            .is_err()
    );
    assert!(
        store
            .record_account_checkpoint(&stale, initial.account_checkpoint())
            .is_err()
    );
    assert_eq!(fs::read(store.root.join(INDEX)).unwrap(), retained);
    let DraftStatus::Ready(latest) = store.review().unwrap() else {
        panic!("newer checkpoint lost");
    };
    assert_eq!(latest.account_checkpoint(), Some(&newer));
    assert_eq!(latest.resume().unwrap().public_key(), prepared.public_key());
    latest
        .with_current(|| {
            assert!(
                store
                    .record_account_checkpoint(&stale, latest.account_checkpoint())
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(fs::read(store.root.join(INDEX)).unwrap(), retained);
    let guard = latest.acquire_current().unwrap();
    assert!(
        store
            .record_account_checkpoint(&stale, latest.account_checkpoint())
            .is_err()
    );
    drop(guard);
    let renewed_guard = latest.acquire_current().unwrap();
    drop(renewed_guard);
    let mut changed_choices = choices();
    changed_choices.device_label = "Updated laptop".into();
    store
        .update_choices_for(&prepared, &changed_choices)
        .unwrap();
    assert_eq!(latest.resume().unwrap().public_key(), prepared.public_key());
}

#[test]
fn queued_public_choice_update_is_bound_to_its_original_retained_preparation() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let store = DraftStore::open(&namespace(&root)).unwrap();
    let first = store.reserve(&choices(), IdentityKind::New).unwrap();
    let old = Arc::new(
        PreparedIdentity::create(
            &first.identity_directory(),
            "Old",
            [1, 2, 3],
            IdentityStorage::Files,
        )
        .unwrap(),
    );
    store.acknowledge(&first, &old, None).unwrap();
    let DraftStatus::Ready(inventory_preparation) = store.review().unwrap() else {
        panic!("missing inventory preparation")
    };
    drop(inventory_preparation.acquire_preparation_current().unwrap());
    store.retire().unwrap();
    assert!(inventory_preparation.acquire_preparation_current().is_err());
    let newest_choices = PublicChoices {
        device_label: "Current device".into(),
        purpose: install_profile_model::default_purpose(),
        ..choices()
    };
    let second = store.reserve(&newest_choices, IdentityKind::New).unwrap();
    let current = Arc::new(
        PreparedIdentity::create(
            &second.identity_directory(),
            "Current",
            [4, 5, 6],
            IdentityStorage::Files,
        )
        .unwrap(),
    );
    store.acknowledge(&second, &current, None).unwrap();
    assert!(inventory_preparation.acquire_preparation_current().is_err());
    let before = fs::read(store.root.join(INDEX)).unwrap();
    let stale = PublicChoices {
        device_label: "Old queued choice".into(),
        purpose: install_profile_model::default_purpose(),
        ..choices()
    };
    assert!(store.update_choices_for(&old, &stale).is_err());
    assert_eq!(fs::read(store.root.join(INDEX)).unwrap(), before);
    let DraftStatus::Ready(ready) = store.review().unwrap() else {
        panic!("current preparation missing");
    };
    assert_eq!(ready.choices(), &newest_choices);
    assert_eq!(ready.resume().unwrap().public_key(), current.public_key());
    assert!(first.identity_directory().join("primary.json").is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn committed_account_exclusion_reopens_same_identity_without_declined_checkpoint() {
    use super::super::accounts::{checkpoint, checkpoint_fixture_for_journey};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    for fresh in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let path = namespace(&root);
        let store = DraftStore::open(&path).unwrap();
        let pending = store.reserve(&choices(), IdentityKind::New).unwrap();
        let identity = Arc::new(
            PreparedIdentity::create(
                &pending.identity_directory(),
                "Ada",
                [1, 2, 3],
                IdentityStorage::Files,
            )
            .unwrap(),
        );
        store.acknowledge(&pending, &identity, None).unwrap();
        let accounts = checkpoint_fixture_for_journey(&root);
        let source_vault = accounts::vault_dir_in(&root.join("source")).join("vault.enc");
        let original_source = fs::read(&source_vault).unwrap();
        let handle = accounts.prepared_handle().unwrap();
        let account_root = pending.operation_directory().join("accounts");
        let receipt = checkpoint::save(
            &handle,
            &account_root,
            &pending.pointer.operation,
            identity.public_key(),
            b"original chosen preparation password",
        )
        .unwrap();
        assert_eq!(receipt.accounts(), 2);
        let DraftStatus::Ready(inventory_preparation) = store.review().unwrap() else {
            panic!("missing inventory preparation")
        };
        store.record_account_checkpoint(&receipt, None).unwrap();
        // Home inventory storage belongs to the preparation, while restoring
        // accounts must still reject a changed checkpoint generation.
        assert!(inventory_preparation.acquire_current().is_err());
        drop(inventory_preparation.acquire_preparation_current().unwrap());
        let DraftStatus::Ready(before) = store.review().unwrap() else {
            panic!("missing preparation")
        };
        let encrypted = fs::read(account_root.join(receipt.filename())).unwrap();
        let key = identity.public_key();
        let mut declined = choices();
        if fresh {
            declined.migrate = Some(false);
        } else {
            declined.categories[2] = false;
        }
        // Merely editing/cancelling a pending review doesn't commit a new choice.
        assert_eq!(before.account_checkpoint(), Some(&receipt));
        assert_eq!(
            checkpoint::restore(
                &account_root,
                &receipt,
                b"original chosen preparation password"
            )
            .unwrap()
            .count(),
            2
        );
        store.update_choices_for(&identity, &declined).unwrap();
        assert_eq!(
            before.acquire_current().err().unwrap().kind(),
            io::ErrorKind::WouldBlock
        );
        assert!(store.record_account_checkpoint(&receipt, None).is_err());
        drop(accounts);
        drop(identity);
        drop(store);
        let reopened = DraftStore::open(&path).unwrap();
        let DraftStatus::Ready(after) = reopened.review().unwrap() else {
            panic!("lost identity")
        };
        assert_eq!(after.resume().unwrap().public_key(), key);
        assert_eq!(after.choices(), &declined);
        assert!(after.account_checkpoint().is_none());
        // Drafts written by the prior implementation may still contain the
        // old receipt alongside an excluded choice. Preserve identity recovery
        // while refusing to expose that declined credential selection.
        let mut legacy = reopened.read().unwrap().unwrap();
        legacy.account_checkpoint = Some(receipt.clone());
        reopened.write(&legacy).unwrap();
        let DraftStatus::Ready(legacy_ready) = reopened.review().unwrap() else {
            panic!("legacy identity lost")
        };
        assert!(legacy_ready.account_checkpoint().is_none());
        assert_eq!(legacy_ready.resume().unwrap().public_key(), key);
        assert_eq!(fs::read(source_vault).unwrap(), original_source);
        assert_eq!(
            fs::read(account_root.join(receipt.filename())).unwrap(),
            encrypted
        );
    }
}
