use super::*;
use crate::setup::{
    account_transfer::Connector,
    portable_transfer::{ExportReview, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{ACCOUNTS_FILE, IDENTITY_FILE, LIMITS, Manifest},
};
use std::{fs, os::unix::fs::PermissionsExt};
use tilo_identity_host::provisioning::IdentityStorage;
const PASSWORD: &[u8] = b"several unrelated installation words";
fn fixture(root: &Path) -> (PathBuf, [u8; 32], accounts::AccountId, PathBuf) {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let staging = root.join("staging");
    fs::create_dir(&staging).unwrap();
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700)).unwrap();
    let identity = PreparedIdentity::create(
        &root.join("identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let source = root.join("source-accounts");
    let connector = Connector::new(source.clone());
    let id = connector
        .connect_webdav(
            "Files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("selected-private-password".into()),
        )
        .unwrap();
    connector
        .export_selected_accounts(
            std::slice::from_ref(&id),
            &staging.join(ACCOUNTS_FILE),
            PASSWORD,
            b"helper-operation",
        )
        .unwrap();
    Manifest::new(
        "helper-operation".into(),
        identity.public_key(),
        vec![id.as_str().into()],
    )
    .unwrap()
    .write(&staging)
    .unwrap();
    let package = root.join("setup.sealed");
    ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
    (package, identity.public_key(), id, source)
}
fn args(root: &Path, package: &Path, quarantine: &str) -> Vec<OsString> {
    [
        ("--package", package.to_path_buf()),
        ("--quarantine", root.join(quarantine)),
        ("--identity-dir", root.join("identities")),
        ("--config-root", root.join("tilo")),
    ]
    .into_iter()
    .flat_map(|(key, value)| [OsString::from(key), value.into_os_string()])
    .collect()
}
#[test]
fn headless_entry_authenticates_imports_and_retries_without_remint_or_secret_response() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (package, key, id, source) = fixture(&root);
    let ciphertext = fs::read(&package).unwrap();
    let original = fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let mut output = Vec::new();
    assert!(
        run(
            args(&root, &package, "wrong"),
            &mut &b"wrong-password"[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert!(!root.join("identities").exists());
    assert!(!root.join("tilo").exists());
    for quarantine in ["first", "retry"] {
        output.clear();
        run(
            args(&root, &package, quarantine),
            &mut &PASSWORD[..],
            &mut output,
        )
        .unwrap();
        let receipt: Receipt = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            receipt,
            Receipt {
                identity_public_key: key,
                account_count: 1,
                current_identity: None,
                service_sessions: None,
                device_label: String::new(),
                portable_home: None,
                portable_flatpak: None,
                browser: None,
                mail: Vec::new(),
            }
        );
        assert!(
            !output
                .windows(b"selected-private-password".len())
                .any(|part| part == b"selected-private-password")
        );
        assert_eq!(
            runtime_identity::load_from_path(root.join("identities/primary.json"))
                .unwrap()
                .group_public_key(),
            key
        );
        assert!(
            accounts::AccountStore::load_in(&root.join("tilo"))
                .get(&id)
                .is_some()
        );
        assert!(!root.join("identities/active").exists());
    }
    assert_eq!(fs::read(&package).unwrap(), ciphertext);
    assert_eq!(
        fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        original
    );
    assert!(root.join("first/home").exists());
    assert!(root.join("retry/home").exists());
    for secret in [vec![b'x'; 4097], vec![0xff; 15], vec![]] {
        output.clear();
        assert!(
            run(
                args(&root, &package, "invalid-secret"),
                &mut &secret[..],
                &mut output
            )
            .is_err()
        );
        assert!(output.is_empty());
    }
}
#[test]
fn account_conflict_retains_identity_quarantine_and_no_receipt_until_full_retry_succeeds() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (package, key, id, source) = fixture(&root);
    let target = root.join("tilo");
    let source_connector = Connector::new(source);
    let sealed = source_connector
        .seal_selected_accounts_with_password(
            std::slice::from_ref(&id),
            PASSWORD,
            b"helper-operation",
        )
        .unwrap();
    Connector::new(target.clone())
        .import_password_sealed_accounts(&sealed, PASSWORD, b"helper-operation")
        .unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o700)).unwrap();
    let mut vault = accounts::SecretVault::open_in(&target);
    let original = vault.get(&id).unwrap();
    let mut conflicting = original.clone();
    conflicting.password = "unrelated-target-password".into();
    vault.put(&id, &conflicting);
    vault.try_save().unwrap();
    let before = fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap();
    let mut output = Vec::new();
    assert!(
        run(
            args(&root, &package, "incomplete"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert!(root.join("incomplete/home").exists());
    assert_eq!(
        runtime_identity::load_from_path(root.join("identities/primary.json"))
            .unwrap()
            .group_public_key(),
        key
    );
    assert_eq!(
        fs::read(accounts::vault_dir_in(&target).join("vault.enc")).unwrap(),
        before
    );
    vault.put(&id, &original);
    vault.try_save().unwrap();
    run(
        args(&root, &package, "completed-retry"),
        &mut &PASSWORD[..],
        &mut output,
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Receipt>(&output)
            .unwrap()
            .account_count,
        1
    );
}

fn authenticate_configured_profile(root: &Path) {
    use sha2::{Digest, Sha256};
    let staging = root.join("staging");
    let mut manifest: Manifest = serde_json::from_slice(
        &fs::read(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    let mut draft = crate::imager_model::InstallProfile::default();
    draft.user.username = "ada".into();
    draft.user.full_name = "Ada Lovelace".into();
    manifest.install_profile =
        Some(crate::setup::prepared_setup::PublicInstallProfile::from_draft(&draft).unwrap());
    fs::remove_file(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap();
    manifest.write(&staging).unwrap();
    let configured_package = root.join("configured.sealed");
    ExportReview::review(
        &staging,
        &configured_package,
        Purpose::PreparedSetup,
        LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    let configured_bytes = fs::read(&configured_package).unwrap();
    let configured_receipt = root.join("configured-receipt.json");
    fs::write(&configured_receipt, serde_json::to_vec(&serde_json::json!({"schema":1,"sealed_bytes":configured_bytes.len(),"ciphertext_sha256":hex::encode(Sha256::digest(&configured_bytes))})).unwrap()).unwrap();
    let configured_args = [
        ("--package", configured_package),
        ("--receipt", configured_receipt),
        ("--quarantine", root.join("configured-auth")),
    ]
    .into_iter()
    .flat_map(|(key, value)| [OsString::from(key), value.into_os_string()]);
    let mut configured_output = Vec::new();
    authenticate::run(configured_args, &mut &PASSWORD[..], &mut configured_output).unwrap();
    let configured: serde_json::Value = serde_json::from_slice(&configured_output).unwrap();
    assert_eq!(configured["install_profile"]["username"], "ada");
    assert_eq!(configured["install_profile"]["full_name"], "Ada Lovelace");
    assert!(!root.join("identities").exists());
    assert!(!root.join("tilo").exists());
}

#[test]
fn authenticate_install_checks_exact_ciphertext_without_importing_or_activating() {
    use sha2::{Digest, Sha256};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (package, key, _, _) = fixture(&root);
    let bytes = fs::read(&package).unwrap();
    let digest = hex::encode(Sha256::digest(&bytes));
    let receipt = root.join("expectation.json");
    fs::write(
        &receipt,
        serde_json::to_vec(
            &serde_json::json!({"schema":1,"sealed_bytes":bytes.len(),"ciphertext_sha256":digest}),
        )
        .unwrap(),
    )
    .unwrap();
    let arguments = |name: &str| {
        [
            ("--package", package.clone()),
            ("--receipt", receipt.clone()),
            ("--quarantine", root.join(name)),
        ]
        .into_iter()
        .flat_map(|(key, value)| [OsString::from(key), value.into_os_string()])
        .collect::<Vec<_>>()
    };
    let mut output = Vec::new();
    assert!(
        authenticate::run(
            arguments("wrong-auth"),
            &mut &b"wrong password words"[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    authenticate::run(arguments("authenticated"), &mut &PASSWORD[..], &mut output).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(value["identity_public_key"], serde_json::json!(key));
    assert_eq!(value["setup_binding"], "helper-operation");
    assert!(value["install_profile"].is_null());
    assert!(!root.join("identities").exists());
    assert!(!root.join("tilo").exists());
    assert_eq!(fs::read(&package).unwrap(), bytes);
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains(std::str::from_utf8(PASSWORD).unwrap())
    );
    authenticate_configured_profile(&root);
    let mut changed = bytes;
    changed[20] ^= 1;
    fs::write(&package, changed).unwrap();
    let mut refused = Vec::new();
    assert!(
        authenticate::run(arguments("changed-auth"), &mut &PASSWORD[..], &mut refused).is_err()
    );
    assert!(refused.is_empty());
    assert!(!root.join("changed-auth").exists());
}

#[test]
fn authenticated_device_label_survives_actual_identity_and_account_import() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (_, key, id, source) = fixture(&root);
    let source_before = fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let staging = root.join("staging");
    let manifest_path = staging.join(crate::setup::prepared_setup::MANIFEST_FILE);
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let draft = crate::imager_model::InstallProfile {
        device_label: "Ada’s workstation 🌿".into(),
        ..Default::default()
    };
    manifest.install_profile =
        Some(crate::setup::prepared_setup::PublicInstallProfile::from_draft(&draft).unwrap());
    fs::remove_file(&manifest_path).unwrap();
    manifest.write(&staging).unwrap();
    let package = root.join("labeled.sealed");
    ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
    let original_package = fs::read(&package).unwrap();
    let mut output = Vec::new();
    run(
        args(&root, &package, "labeled-quarantine"),
        &mut &PASSWORD[..],
        &mut output,
    )
    .unwrap();
    let receipt: Receipt = serde_json::from_slice(&output).unwrap();
    assert_eq!(receipt.device_label, draft.device_label);
    assert_eq!(receipt.identity_public_key, key);
    assert_eq!(receipt.account_count, 1);
    assert_eq!(
        runtime_identity::load_from_path(root.join("identities/primary.json"))
            .unwrap()
            .group_public_key(),
        key
    );
    assert!(
        accounts::AccountStore::load_in(&root.join("tilo"))
            .get(&id)
            .is_some()
    );
    assert_eq!(fs::read(&package).unwrap(), original_package);
    assert_eq!(
        fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        source_before
    );
}

#[test]
fn authenticated_retained_plan_preserves_consent_and_refuses_unattested_source() {
    use sha2::{Digest, Sha256};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fixture(&root);
    let staging = root.join("staging");
    let manifest_path = staging.join(crate::setup::prepared_setup::MANIFEST_FILE);
    let mut manifest: Manifest =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let value = serde_json::json!({
        "schema":1,"operation_id":uuid::Uuid::new_v4().to_string(),
        "categories":{"home":true,"flatpak":false,"accounts":false},
        "home_policy":"access-read-write",
        "home":{
            "volume":{
                "disk_guid":"11111111-1111-4111-8111-111111111111",
                "disk_bytes":68_719_476_736_u64,"sector_bytes":512,
                "partition_guid":"22222222-2222-4222-8222-222222222222",
                "first_lba":2048,"last_lba":1_048_575,
                "filesystem_uuid":"33333333-3333-4333-8333-333333333333",
                "filesystem":"ext4"
            },
            "relative":{"platform":"Unix","units":b"home/person"},
            "inode":1234,"original_uid":1000,"subvolume":null
        }
    });
    let plan =
        crate::setup::migration_handoff::Plan::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    manifest.retained_home =
        Some(crate::setup::prepared_setup::RetainedHomePlan::new(plan).unwrap());
    fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let package = root.join("retained.sealed");
    ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
    let bytes = fs::read(&package).unwrap();
    let receipt = root.join("retained-receipt.json");
    fs::write(
        &receipt,
        serde_json::to_vec(&serde_json::json!({
            "schema":1,"sealed_bytes":bytes.len(),
            "ciphertext_sha256":hex::encode(Sha256::digest(&bytes))
        }))
        .unwrap(),
    )
    .unwrap();
    let args = [
        ("--package", package.clone()),
        ("--receipt", receipt),
        ("--quarantine", root.join("retained-auth")),
    ]
    .into_iter()
    .flat_map(|(key, path)| [OsString::from(key), path.into_os_string()])
    .collect::<Vec<_>>();
    let mut output = Vec::new();
    authenticate::run(args.clone(), &mut &PASSWORD[..], &mut output).unwrap();
    let public: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(public["retained_home"], value);
    assert_eq!(fs::read(&package).unwrap(), bytes);
    assert!(!root.join("target-home").exists());
    let mut verify_args = args;
    verify_args.push(OsString::from("--verify-retained-source"));
    for (index, value) in verify_args.iter_mut().enumerate() {
        if index == 5 {
            *value = root.join("unattested-source-auth").into_os_string();
        }
    }
    let mut refused = Vec::new();
    assert!(authenticate::run(verify_args, &mut &PASSWORD[..], &mut refused).is_err());
    assert!(refused.is_empty());
    assert_eq!(fs::read(&package).unwrap(), bytes);
}

#[test]
fn home_capability_probe_accepts_only_gnu_tar_with_sparse_pax_support() {
    assert!(super::validate_tar_version(b"tar (GNU tar) 1.35\nCopyright\n").is_ok());
    for bad in [
        b"bsdtar 3.7.7\n".as_slice(),
        b"tar (GNU tar) 1.29\n",
        b"tar (GNU tar) 1.35-foreign\n",
        b"tar (GNU tar) 1\n",
    ] {
        assert!(super::validate_tar_version(bad).is_err());
    }
    let mut output = Vec::new();
    assert!(
        super::home_consumer_capability([std::ffi::OsString::from("--path")], &mut output).is_err()
    );
    assert!(output.is_empty());
}

#[test]
fn authenticate_completed_declaration_emits_exact_application_binding_without_import() {
    use sha2::{Digest, Sha256};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (original, key, _, _) = fixture(&root);
    let original_bytes = fs::read(&original).unwrap();
    let staging = root.join("staging");
    let mut manifest: Manifest = serde_json::from_slice(
        &fs::read(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap(),
    )
    .unwrap();
    let limits = crate::setup::portable_transfer::Limits {
        plaintext_bytes: 2048,
        entries: 32,
        metadata_bytes: 1024,
    };
    let home = crate::setup::portable_home_media::Expectation {
        schema: 1,
        operation: uuid::Uuid::new_v4().to_string(),
        purpose: Purpose::Home,
        filename: String::new(),
        setup_binding: manifest.setup_binding.clone(),
        sealed_bytes: 500,
        plaintext_bytes: 256,
        entries: 2,
        runtime_sockets: 0,
        ciphertext_sha256: [7; 32],
        limits,
    };
    let home = crate::setup::portable_home_media::Expectation {
        filename: format!("home-{}.sealed", home.operation),
        ..home
    };
    let operation = uuid::Uuid::new_v4().to_string();
    let apps = crate::setup::flatpak_transfer::portable::Expectation {
        schema: 1,
        consumer_protocol: 1,
        filename: format!("applications-{operation}.sealed"),
        operation,
        purpose: Purpose::FlatpakApplications,
        setup_binding: manifest.setup_binding.clone(),
        sealed_bytes: 600,
        plaintext_bytes: 300,
        entries: 3,
        ciphertext_sha256: [8; 32],
        manifest_sha256: [9; 32],
        limits,
    };
    manifest.portable_home = Some(crate::setup::prepared_setup::PortableHomePlan {
        schema: 1,
        selection: crate::setup::prepared_setup::PortableHomeSelection::FullHome,
        applications: true,
    });
    manifest.completed_home = Some(home.clone());
    manifest.portable_flatpak = Some(apps.clone());
    fs::remove_file(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap();
    manifest.write(&staging).unwrap();
    let package = root.join("completed.sealed");
    ExportReview::review(&staging, &package, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
    let bytes = fs::read(&package).unwrap();
    let expectation = root.join("completed-receipt.json");
    fs::write(&expectation,serde_json::to_vec(&serde_json::json!({"schema":1,"sealed_bytes":bytes.len(),"ciphertext_sha256":hex::encode(Sha256::digest(&bytes))})).unwrap()).unwrap();
    let arguments = |quarantine: &str| {
        [
            ("--package", package.clone()),
            ("--receipt", expectation.clone()),
            ("--quarantine", root.join(quarantine)),
        ]
        .into_iter()
        .flat_map(|(key, path)| [OsString::from(key), path.into_os_string()])
    };
    let mut output = Vec::new();
    authenticate::run(arguments("completed-auth"), &mut &PASSWORD[..], &mut output).unwrap();
    let receipt: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        receipt["completed_home"],
        serde_json::to_value(home).unwrap()
    );
    assert_eq!(
        receipt["completed_applications"],
        serde_json::to_value(apps).unwrap()
    );
    assert_eq!(receipt["identity_public_key"], serde_json::json!(key));
    assert!(
        authenticate::run(
            arguments("wrong-completed-auth"),
            &mut &b"wrong words"[..],
            &mut Vec::new()
        )
        .is_err()
    );
    assert_eq!(fs::read(&original).unwrap(), original_bytes);
    assert_eq!(fs::read(&package).unwrap(), bytes);
    assert!(!root.join("identities").exists());
    assert!(!root.join("tilo").exists());
    // This tests password-authenticated public declaration transport. Only the
    // genuine independent worker token may construct completed producer output.
}

#[test]
fn unseal_handoff_refuses_a_sidecar_for_another_package_before_any_tpm_operation() {
    use crate::setup::prepared_tpm::{
        PCRS, PLATFORM_PCRS, RECEIVER_UID, SCHEMA, SealedPreparation, SignedTarget, b64,
    };
    let directory = tempfile::tempdir().unwrap();
    let sidecar = directory.path().join("tpm-handoff.json");
    let package = directory.path().join("prepared-setup.sealed");
    fs::write(&package, b"the exact package the sidecar does not name").unwrap();
    let policy = SignedTarget {
        schema: SCHEMA,
        generation: 1,
        pcr_bank: "sha256".into(),
        pcrs: PCRS.to_vec(),
        platform_pcrs: PLATFORM_PCRS.to_vec(),
        approved_policy: b64(&[1; 32]),
        signature: b64(&[2; 256]),
        target_uki_sha256: [3; 32],
        authority_sha256: [4; 32],
        label: "unseal preflight fixture".into(),
    };
    let mut sealed = SealedPreparation {
        schema: SCHEMA,
        policy,
        source_public_key: [5; 32],
        package_sha256: [6; 32],
        receiver_uid: RECEIVER_UID,
        receiver_tpm_name: b64(&[7; 32]),
        key_name: b64(&[8; 32]),
        sealed_public: b64(&[9; 32]),
        sealed_private: b64(&[10; 32]),
        policy_digest: b64(&[11; 32]),
        seal_nonce: b64(&[12; 32]),
        binding_sha256: String::new(),
    };
    sealed.seal_binding().unwrap();
    fs::write(&sidecar, sealed.encode().unwrap()).unwrap();
    let mut output = Vec::new();
    let error = unseal_handoff(
        [
            OsString::from("--sidecar"),
            sidecar.into_os_string(),
            OsString::from("--package"),
            package.into_os_string(),
        ],
        &mut output,
    )
    .expect_err("a sidecar naming another package must stop before the TPM");
    assert!(error.to_string().contains("another package"));
    assert!(output.is_empty());
}
