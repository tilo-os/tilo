use super::*;

fn connect(root: &std::path::Path, name: &str, password: &str) -> AccountId {
    let dav_server = crate::account_transfer::dav_readiness::test_server();
    Connector::new(root.to_owned())
        .connect_webdav(
            name,
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new(password.to_string()),
        )
        .unwrap()
}

#[test]
fn selected_account_transport_reseals_fresh_custody_without_copying_source_key() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    let chosen = connect(&source, "Selected", "selected-private-app-password");
    let unselected = connect(&source, "Not selected", "unselected-private-app-password");
    let unrelated = connect(
        &target,
        "Existing target",
        "existing-target-private-password",
    );
    let source_store = std::fs::read(accounts::store_path_in(&source)).unwrap();
    let source_key = std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap();
    let source_cipher = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let key = SymmetricKey::generate().unwrap();
    let sealed = Connector::new(source.clone())
        .seal_selected_accounts(std::slice::from_ref(&chosen), &key, b"setup-operation-1")
        .unwrap();
    assert!(
        !sealed
            .as_bytes()
            .windows(source_key.len())
            .any(|window| window == source_key)
    );
    assert!(!format!("{sealed:?}").contains("private-app-password"));
    let parsed = SealedAccounts::from_bytes(sealed.as_bytes()).unwrap();
    let connector = Connector::new(target.clone());
    for _ in 0..2 {
        assert_eq!(
            connector
                .import_sealed_accounts(&parsed, &key, b"setup-operation-1")
                .unwrap(),
            vec![chosen.clone()]
        );
    }
    let store = accounts::AccountStore::load_in(&target);
    assert_eq!(store.len(), 2);
    assert!(store.get(&chosen).is_some());
    assert!(store.get(&unrelated).is_some());
    assert!(store.get(&unselected).is_none());
    let vault = SecretVault::open_in(&target);
    assert_eq!(
        vault.get(&chosen).unwrap().password,
        "selected-private-app-password"
    );
    assert_eq!(
        vault.get(&unrelated).unwrap().password,
        "existing-target-private-password"
    );
    assert!(vault.get(&unselected).is_none());
    assert_ne!(
        std::fs::read(accounts::vault_dir_in(&target).join("vault.key")).unwrap(),
        source_key
    );
    assert_eq!(
        std::fs::read(accounts::store_path_in(&source)).unwrap(),
        source_store
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_cipher
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap(),
        source_key
    );
}

#[test]
fn wrong_setup_key_tampering_and_empty_binding_never_write_destination() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("never-created");
    let id = connect(&source, "Selected", "private-app-password");
    let key = SymmetricKey::generate().unwrap();
    let source_connector = Connector::new(source);
    let sealed = source_connector
        .seal_selected_accounts(std::slice::from_ref(&id), &key, b"setup-1")
        .unwrap();
    let target_connector = Connector::new(target.clone());
    assert!(
        target_connector
            .import_sealed_accounts(&sealed, &key, b"setup-2")
            .is_err()
    );
    assert!(
        target_connector
            .import_sealed_accounts(&sealed, &SymmetricKey::generate().unwrap(), b"setup-1")
            .is_err()
    );
    assert!(
        target_connector
            .import_sealed_accounts(&sealed, &key, b"")
            .is_err()
    );
    let mut tampered = sealed.as_bytes().to_vec();
    *tampered.last_mut().unwrap() ^= 1;
    let tampered = SealedAccounts::from_bytes(&tampered).unwrap();
    assert!(
        target_connector
            .import_sealed_accounts(&tampered, &key, b"setup-1")
            .is_err()
    );
    assert!(!target.exists());
    assert!(
        source_connector
            .seal_selected_accounts(&[id.clone(), id], &key, b"setup-1")
            .is_err()
    );
    assert!(SealedAccounts::from_bytes(&vec![0; MAX_SEALED + 1]).is_err());
}

#[test]
fn later_conflicting_account_refuses_entire_selection_before_any_import() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    let first = connect(&source, "First selected", "first-private-password");
    let conflicting = connect(&source, "Second selected", "second-private-password");
    let target_connector = Connector::new(target.clone());
    target_connector.import_tilo(&source, &conflicting).unwrap();
    let mut store = accounts::AccountStore::load_in(&target);
    let mut existing = store.get(&conflicting).unwrap().clone();
    existing.display_name = "Changed by target owner".into();
    store.upsert(existing);
    store.save_in(&target).unwrap();
    let before = std::fs::read(accounts::store_path_in(&target)).unwrap();
    let key = SymmetricKey::generate().unwrap();
    let source_connector = Connector::new(source.clone());
    let sealed = source_connector
        .seal_selected_accounts(&[first.clone(), conflicting], &key, b"setup-1")
        .unwrap();
    assert_eq!(
        target_connector.import_sealed_accounts(&sealed, &key, b"setup-1"),
        Err(ConnectionError::AlreadyPresent)
    );
    assert_eq!(
        std::fs::read(accounts::store_path_in(&target)).unwrap(),
        before
    );
    assert!(
        accounts::AccountStore::load_in(&target)
            .get(&first)
            .is_none()
    );
    assert!(SecretVault::open_in(&target).get(&first).is_none());
    let mut store = accounts::AccountStore::load_in(&source);
    let mut contributed = store.get(&first).unwrap().clone();
    contributed.owner = Some("another-collaborator".into());
    store.upsert(contributed);
    store.save_in(&source).unwrap();
    assert!(
        source_connector
            .seal_selected_accounts(&[first], &key, b"setup-1")
            .is_err()
    );
}

#[test]
fn partial_selection_retry_and_corrupt_custody_are_handled_without_overwrite() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    let first = connect(&source, "First", "first-private-password");
    let second = connect(&source, "Second", "second-private-password");
    let unrelated = connect(&target, "Existing", "existing-private-password");
    let source_connector = Connector::new(source.clone());
    let target_connector = Connector::new(target.clone());
    let key = SymmetricKey::generate().unwrap();
    let partial = source_connector
        .seal_selected_accounts(std::slice::from_ref(&first), &key, b"setup-1")
        .unwrap();
    target_connector
        .import_sealed_accounts(&partial, &key, b"setup-1")
        .unwrap();
    let all = source_connector
        .seal_selected_accounts(&[first.clone(), second.clone()], &key, b"setup-1")
        .unwrap();
    // Simulate the canonical vault save succeeding before metadata save:
    // exact retry must finish this selected account, retaining unrelated data.
    let mut partial_vault = SecretVault::open_in(&target);
    let source_secret = SecretVault::open_in(&source).get(&second).unwrap();
    partial_vault.put(&second, &source_secret);
    partial_vault.try_save().unwrap();
    assert_eq!(
        target_connector
            .import_sealed_accounts(&all, &key, b"setup-1")
            .unwrap(),
        vec![first.clone(), second]
    );
    assert_eq!(accounts::AccountStore::load_in(&target).len(), 3);
    assert_eq!(
        SecretVault::open_in(&target)
            .get(&unrelated)
            .unwrap()
            .password,
        "existing-private-password"
    );
    let metadata = std::fs::read(accounts::store_path_in(&target)).unwrap();
    let corrupt = accounts::vault_dir_in(&target).join("vault.enc");
    std::fs::write(&corrupt, b"corrupt-existing-vault").unwrap();
    assert_eq!(
        target_connector.import_sealed_accounts(&all, &key, b"setup-1"),
        Err(ConnectionError::StorageUnavailable)
    );
    assert_eq!(std::fs::read(&corrupt).unwrap(), b"corrupt-existing-vault");
    assert_eq!(
        std::fs::read(accounts::store_path_in(&target)).unwrap(),
        metadata
    );
    std::fs::write(
        accounts::vault_dir_in(&source).join("vault.enc"),
        b"corrupt-source-vault",
    )
    .unwrap();
    assert!(
        source_connector
            .seal_selected_accounts(&[first], &key, b"setup-1")
            .is_err()
    );
}

#[test]
fn authenticated_foreign_owner_bundle_is_refused_before_target_creation() {
    let fixture = tempfile::tempdir().unwrap();
    let target = fixture.path().join("never-created");
    let key = SymmetricKey::generate().unwrap();
    let mut account = Account::new(AccountId::new("foreign-account"), "webdav");
    account.owner = Some("another-collaborator".into());
    let bundle = Bundle {
        version: 1,
        entries: vec![Entry {
            account,
            secret: Secret::password("private-collaborator-password"),
        }],
    };
    let plain = Zeroizing::new(serde_json::to_vec(&bundle).unwrap());
    let context = binding(b"setup-1").unwrap();
    let sealed = SealedAccounts(
        envelope::seal(
            &key,
            Context::new(Label::MigrationPackage, &context),
            &plain,
        )
        .unwrap(),
    );
    assert_eq!(
        Connector::new(target.clone()).import_sealed_accounts(&sealed, &key, b"setup-1"),
        Err(ConnectionError::InvalidConfiguration)
    );
    assert!(!target.exists());
}

#[test]
fn selected_serialization_limit_rejects_account_before_emitting_transport() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let id = connect(&source, "Oversized account", "bounded-source-password");
    // Existing/external source custody can contain a credential too large for
    // migration. Establish a genuine DAV connection first; do not send the
    // serialization-limit subject as an oversized HTTP authentication header.
    let mut vault = SecretVault::open_in(&source);
    vault.put(&id, &Secret::password("x".repeat(MAX_PLAINTEXT)));
    vault.try_save().unwrap();
    drop(vault);
    assert_eq!(
        account_custody::password_credential(&SecretVault::open_in(&source).get(&id).unwrap())
            .len(),
        MAX_PLAINTEXT
    );
    let before = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    assert!(matches!(
        Connector::new(source.clone()).seal_selected_accounts(
            &[id],
            &SymmetricKey::generate().unwrap(),
            b"setup-1",
        ),
        Err(ConnectionError::InvalidConfiguration)
    ));
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        before
    );
}

const TRANSPORT_PASSWORD: &[u8] = b"a private transport password";

#[test]
fn password_artifact_roundtrip_reseals_selected_grant_without_source_key_transport() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("target");
    let selected = connect(&source, "Selected", "selected-private-password");
    let unselected = connect(&source, "Unselected", "unselected-private-password");
    let source_store = std::fs::read(accounts::store_path_in(&source)).unwrap();
    let source_key = std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap();
    let source_vault = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let connector = Connector::new(source.clone());
    let artifact = connector
        .seal_selected_accounts_with_password(
            std::slice::from_ref(&selected),
            TRANSPORT_PASSWORD,
            b"operation-1",
        )
        .unwrap();
    let fresh = connector
        .seal_selected_accounts_with_password(
            std::slice::from_ref(&selected),
            TRANSPORT_PASSWORD,
            b"operation-1",
        )
        .unwrap();
    assert_ne!(artifact.as_bytes(), fresh.as_bytes());
    assert!(
        !artifact
            .as_bytes()
            .windows(source_key.len())
            .any(|window| window == source_key)
    );
    assert!(!format!("{artifact:?}").contains("password"));
    let readback = PasswordSealedAccounts::from_bytes(artifact.as_bytes()).unwrap();
    let imported = Connector::new(target.clone())
        .import_password_sealed_accounts(&readback, TRANSPORT_PASSWORD, b"operation-1")
        .unwrap();
    assert_eq!(imported, vec![selected.clone()]);
    assert!(
        accounts::AccountStore::load_in(&target)
            .get(&unselected)
            .is_none()
    );
    assert_eq!(
        SecretVault::open_in(&target)
            .get(&selected)
            .unwrap()
            .password,
        "selected-private-password"
    );
    assert_ne!(
        std::fs::read(accounts::vault_dir_in(&target).join("vault.key")).unwrap(),
        source_key
    );
    assert_eq!(
        std::fs::read(accounts::store_path_in(&source)).unwrap(),
        source_store
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.key")).unwrap(),
        source_key
    );
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_vault
    );
}

#[test]
fn wrapped_key_binding_tamper_and_expensive_kdf_policy_refuse_before_target_writes() {
    let fixture = tempfile::tempdir().unwrap();
    let source = fixture.path().join("source");
    let target = fixture.path().join("never-created");
    let selected = connect(&source, "Selected", "selected-private-password");
    let artifact = Connector::new(source)
        .seal_selected_accounts_with_password(&[selected], TRANSPORT_PASSWORD, b"operation-1")
        .unwrap();
    let connector = Connector::new(target.clone());
    assert!(
        connector
            .import_password_sealed_accounts(&artifact, b"wrong transport password", b"operation-1")
            .is_err()
    );
    assert!(
        connector
            .import_password_sealed_accounts(&artifact, TRANSPORT_PASSWORD, b"operation-2")
            .is_err()
    );
    let mut changed = artifact.as_bytes().to_vec();
    *changed.last_mut().unwrap() ^= 1;
    let changed = PasswordSealedAccounts::from_bytes(&changed).unwrap();
    assert!(
        connector
            .import_password_sealed_accounts(&changed, TRANSPORT_PASSWORD, b"operation-1")
            .is_err()
    );
    let mut expensive = artifact.as_bytes().to_vec();
    // The wrapped envelope starts at 12; its Argon2 memory field at 8.
    expensive[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(PasswordSealedAccounts::from_bytes(&expensive).is_err());
    assert!(
        PasswordSealedAccounts::from_bytes(&artifact.as_bytes()[..artifact.as_bytes().len() - 1])
            .is_err()
    );
    let mut trailing = artifact.as_bytes().to_vec();
    trailing.push(0);
    assert!(PasswordSealedAccounts::from_bytes(&trailing).is_err());
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn private_publication_is_atomic_create_new_and_symlink_safe() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let source = root.join("source");
    let target = root.join("target");
    let private = root.join("artifacts");
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&private)
        .unwrap();
    let selected = connect(&source, "Selected", "selected-private-password");
    let connector = Connector::new(source.clone());
    let path = private.join("selected.accounts");
    connector
        .export_selected_accounts(
            std::slice::from_ref(&selected),
            &path,
            TRANSPORT_PASSWORD,
            b"operation-1",
        )
        .unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(std::fs::read_dir(&private).unwrap().count(), 1);
    let importer = Connector::new(target.clone());
    for requested in [
        vec![],
        vec![AccountId::new("different-account")],
        vec![selected.clone(), AccountId::new("extra-account")],
        vec![selected.clone(), selected.clone()],
    ] {
        assert!(
            importer
                .import_selected_accounts_matching(
                    &path,
                    TRANSPORT_PASSWORD,
                    b"operation-1",
                    &requested
                )
                .is_err()
        );
        assert!(!target.exists());
    }
    assert_eq!(
        importer
            .import_selected_accounts_matching(
                &path,
                TRANSPORT_PASSWORD,
                b"operation-1",
                std::slice::from_ref(&selected)
            )
            .unwrap(),
        vec![selected.clone()]
    );
    assert_eq!(
        Connector::new(target)
            .import_selected_accounts(&path, TRANSPORT_PASSWORD, b"operation-1")
            .unwrap(),
        vec![selected.clone()]
    );
    let before = std::fs::read(&path).unwrap();
    assert!(
        connector
            .export_selected_accounts(
                std::slice::from_ref(&selected),
                &path,
                TRANSPORT_PASSWORD,
                b"operation-1"
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let old = root.join("original-file");
    std::fs::write(&old, b"original-account-stays").unwrap();
    let symlink = private.join("symlink.accounts");
    std::os::unix::fs::symlink(&old, &symlink).unwrap();
    assert!(
        connector
            .export_selected_accounts(
                std::slice::from_ref(&selected),
                &symlink,
                TRANSPORT_PASSWORD,
                b"operation-1"
            )
            .is_err()
    );
    assert!(PasswordSealedAccounts::read(&symlink).is_err());
    assert_eq!(std::fs::read(&old).unwrap(), b"original-account-stays");
    assert_eq!(std::fs::read_dir(&private).unwrap().count(), 2);
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        connector
            .export_selected_accounts(
                &[selected],
                &source.join("inside-source.accounts"),
                TRANSPORT_PASSWORD,
                b"operation-1"
            )
            .is_err()
    );
    assert!(!source.join("inside-source.accounts").exists());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(PasswordSealedAccounts::read(&path).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn private_artifact_reader_rejects_fifo_without_waiting_for_writer() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    };
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let fifo = root.join("accounts.fifo");
    let path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: a valid NUL-terminated path inside an isolated private fixture.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    // No writer opens the FIFO until the read has answered, so a reader
    // that waited for one would never answer.
    let (answered, answer) = std::sync::mpsc::channel();
    let reading = fifo.clone();
    let reader = std::thread::spawn(move || {
        let _ = answered.send(PasswordSealedAccounts::read(&reading).is_err());
    });
    let refused = answer.recv_timeout(tilo_test_env::hang_bound());
    // Release a regressed blocking reader, so failure cannot hang the suite.
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo);
    reader.join().unwrap();
    assert_eq!(
        refused,
        Ok(true),
        "Private artifact FIFO must be refused without waiting for a writer"
    );
}

#[test]
fn protected_consumer_requirement_follows_exact_authenticated_selected_grants() {
    // A sealed artifact is only read from a private directory; make it one
    // whatever the umask.
    let fixture = tempfile::Builder::new()
        .permissions(<std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700))
        .tempdir()
        .unwrap();
    let source = fixture.path().join("source");
    let copied = fixture.path().join("copied");
    let fresh = connect(&source, "Fresh", "original-private-password");
    let copy = Connector::new(copied);
    copy.import_tilo(&source, &fresh).unwrap();
    let password = b"several unrelated installation words";
    let binding = b"consumer-proof-operation";
    let source_before = std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let fresh_artifact = Connector::new(source.clone())
        .seal_selected_accounts_with_password(std::slice::from_ref(&fresh), password, binding)
        .unwrap();
    assert!(
        !fresh_artifact
            .requires_protected_consumer(password, binding, std::slice::from_ref(&fresh))
            .unwrap()
    );
    let protected = copy
        .seal_selected_accounts_with_password(std::slice::from_ref(&fresh), password, binding)
        .unwrap();
    assert!(
        protected
            .requires_protected_consumer(password, binding, std::slice::from_ref(&fresh))
            .unwrap()
    );
    assert!(
        protected
            .requires_protected_consumer(
                password,
                b"different-operation",
                std::slice::from_ref(&fresh)
            )
            .is_err()
    );
    assert!(
        protected
            .requires_protected_consumer(
                b"different unrelated installation password",
                binding,
                std::slice::from_ref(&fresh)
            )
            .is_err()
    );
    assert!(
        protected
            .requires_protected_consumer(password, binding, &[])
            .is_err()
    );
    let artifact_file = tempfile::NamedTempFile::new_in(fixture.path()).unwrap();
    let artifact_path = artifact_file.path();
    std::fs::write(artifact_path, protected.as_bytes()).unwrap();
    let selected = vec![fresh.as_str().to_owned()];
    assert!(
        PasswordSealedAccounts::inspect_prepared_consumer(
            artifact_path,
            password,
            binding,
            &selected
        )
        .unwrap()
    );
    assert!(
        PasswordSealedAccounts::inspect_prepared_consumer(
            artifact_path,
            password,
            b"different-operation",
            &selected
        )
        .is_err()
    );
    assert!(
        PasswordSealedAccounts::inspect_prepared_consumer(
            artifact_path,
            password,
            binding,
            &["not-selected".to_owned()]
        )
        .is_err()
    );
    // Empty selections omit accounts.sealed in PreparedSetup; the canonical
    // artifact API rejects an empty credential bundle rather than certifying one.
    assert!(matches!(
        copy.seal_selected_accounts_with_password(&[], password, binding),
        Err(ConnectionError::InvalidConfiguration)
    ));
    assert_eq!(
        std::fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_before
    );
}
