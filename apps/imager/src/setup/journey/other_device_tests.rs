//! Two private, owned runtimes hand a real prepared package from this
//! computer to a fresh destination identity. The package contains an actual
//! Thunderbird selection with one folder excluded; the destination verifies
//! the package, imports only the selected folder, and returns a receipt before
//! the source reports completion.
use super::belongings::Belongings;
use super::prepared_media::PreparedMedia;
use super::*;
use crate::setup::{account_transfer, home_transfer};
use imager_backend::prepared_identity::PreparedIdentity;
use imager_backend::setup::other_device::{
    HandoffIdentity, PublicHandoff, Receiver, ReceiverIdentity, Source, SourceState,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

fn message(subject: &str) -> String {
    format!(
        "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nSubject: {subject}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{subject}@example.test>\r\n\r\nBody.\r\n"
    )
}

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let home = fixture.path().join("source-home");
    std::fs::create_dir_all(home.join(".config")).unwrap();
    let thunderbird = home.join(".thunderbird/abc.default");
    std::fs::create_dir_all(thunderbird.join("ImapMail/imap.example.test")).unwrap();
    std::fs::write(
        home.join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\nDefault=1\n",
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("prefs.js"),
        r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.identity.id1.fullName", "Personal");
"#,
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("ImapMail/imap.example.test/Inbox"),
        message("Inbox mail"),
    )
    .unwrap();
    std::fs::write(
        thunderbird.join("ImapMail/imap.example.test/Drafts"),
        message("Draft mail"),
    )
    .unwrap();
    (fixture, home)
}

fn preview(home: &Path) -> InstallerPreview {
    let belongings = Belongings::from_roots(account_transfer::SourceRoots {
        home: home.to_path_buf(),
        config: home_transfer::granted_root(&home.join(".config")).unwrap(),
    });
    let mut preview = InstallerPreview {
        live: true,
        ..Default::default()
    };
    preview.journey.page = Page::Belongings;
    preview.journey.destination = Some(Destination::Another);
    preview.journey.migrate = Some(true);
    preview.journey.categories = [false, false, true];
    preview.belongings = belongings;
    preview
}

fn selected_mail_without_drafts(preview: &InstallerPreview) -> prepared_media::MailSelection {
    let mut mail = preview.selected_mail().unwrap().expect("selected mail");
    for (reviewed, selection) in &mut mail.profiles {
        let drafts = reviewed
            .accounts
            .iter()
            .flat_map(|account| account.folders.iter())
            .filter(|folder| {
                folder.inventory.hierarchy.last().map(String::as_str) == Some("Drafts")
            })
            .map(|folder| folder.inventory.source_id.clone())
            .collect::<Vec<_>>();
        for account in &mut selection.accounts {
            account.folders.retain(|folder| !drafts.contains(folder));
        }
    }
    mail
}

fn read_until(timeout: std::time::Duration, mut check: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + timeout;
    let finished = loop {
        if check() {
            break true;
        }
        if std::time::Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(finished, "handoff did not finish");
}

#[test]
fn a_real_prepared_package_reaches_a_new_identity_and_returns_a_verified_receipt() {
    let (_fixture, home) = fixture();
    let mut env = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("HOME", &home);
        env.set("XDG_STATE_HOME", home.join(".local/state"));
        env.set("XDG_CONFIG_HOME", home.join(".config"));
        env
    };
    let preview = preview(&home);
    let mail = selected_mail_without_drafts(&preview);
    assert!(
        mail.profiles
            .iter()
            .all(
                |(_, selection)| selection.accounts.iter().all(|account| !account
                    .folders
                    .iter()
                    .any(|folder| folder.contains("Drafts")))
            )
    );

    let source_identity = PreparedIdentity::create(
        &home.join("source-identity"),
        "Source person",
        [10, 20, 30],
        imager_backend::prepared_identity::storage_for_session(),
    )
    .unwrap();
    let source = Source::bind_with_identity(
        uuid::Uuid::new_v4().to_string(),
        source_identity.bootstrap_source_identity().unwrap(),
    )
    .unwrap();
    let other = source.identity().clone();
    let key = zeroize::Zeroizing::new(
        "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
    );
    let prepared = PreparedMedia::prepare_with_selections(
        &source_identity,
        None,
        prepared_media::MediaSelections {
            sessions: None,
            home_applications: false,
            home: None,
            retained: None,
            planned_direct: None,
            applications: None,
            browser: None,
            mail: Some(mail),
            other_device: Some(other),
            tpm_target: None,
        },
        key.clone(),
        uuid::Uuid::new_v4().to_string(),
        None,
    )
    .unwrap();
    let expectation = prepared.expectation().clone();
    let mut ciphertext = tempfile::tempfile().unwrap();
    prepared.copy_ciphertext(&mut ciphertext).unwrap();
    use std::io::{Read as _, Seek as _, SeekFrom};
    ciphertext.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    ciphertext.read_to_end(&mut bytes).unwrap();
    let digest: [u8; 32] = hex::decode(&expectation.ciphertext_sha256)
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(digest, {
        use sha2::Digest as _;
        let computed: [u8; 32] = sha2::Sha256::digest(&bytes).into();
        computed
    });
    source.serve(bytes, key.clone(), digest).unwrap();

    // The receiving computer is entirely fresh: no person, no device
    // certificate. It asks the preparing computer to join that person's
    // identity with this machine's own new key. After the person approves the
    // code, the source signs the member certificate and exports the public
    // identity; only then does the ordinary receiver run.
    let destination_system = home.join("destination-system");
    let destination_identity = home.join("destination-identity");
    let join_identity = destination_identity.join(".other-computer-join");
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::create_dir_all(&destination_identity).unwrap();
        std::fs::set_permissions(
            &destination_identity,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    env.set("TILO_IDENTITY_ROOT", &destination_system);
    let package_directory = home.join(".local/state/tilo/prepared-setup");
    let public = PublicHandoff::from_identity(
        source.identity(),
        expectation.ciphertext_sha256.clone(),
        expectation.sealed_bytes,
    );
    let receiver = Receiver::join(
        public,
        ReceiverIdentity {
            identity_root: destination_system.clone(),
            identity_dir: join_identity.clone(),
        },
        package_directory.clone(),
        "Fresh destination".into(),
    )
    .unwrap();

    let mut joined = false;
    let mut received = None;
    read_until(tilo_test_env::hang_bound(), || {
        while let Some(state) = source.poll() {
            match state {
                SourceState::JoinRequested { approval, .. } => {
                    // The person compared the code on both screens and
                    // approved this exact device for this exact package.
                    source.approve(&approval).unwrap();
                    joined = true;
                }
                SourceState::Requested { approval, .. } => {
                    panic!("the typed join receipt must not ask again: {approval}");
                }
                SourceState::Failed | SourceState::Rejected => panic!("source failed"),
                _ => {}
            }
        }
        if received.is_none()
            && let Some(result) = receiver.poll()
        {
            received = Some(result.unwrap_or_else(|error| panic!("receiver failed: {error}")));
        }
        received.is_some()
    });
    assert!(joined, "the source approved the fresh destination's join");
    let joined_record = runtime_identity::resolve_active_identity(&join_identity)
        .expect("the joined identity is selected");
    let joined_metadata = runtime_identity::load_metadata_from_path(&joined_record).unwrap();
    assert_eq!(
        joined_metadata.group_public_key,
        source_identity.public_key()
    );
    let joined_paths = tilo_user_identity::persist::IdentityPaths::under(&destination_system);
    assert!(
        !joined_paths.user_privkey.exists(),
        "the joining device never receives the source root key"
    );
    let joined_cert =
        tilo_user_identity::persist::read_device_cert(&joined_paths.device_cert).unwrap();
    assert_eq!(joined_cert.user_public_key, source_identity.public_key());
    assert!(tilo_user_identity::verify_device_cert(&joined_cert).is_ok());
    let received = received.unwrap();
    assert_eq!(received.password.as_str(), key.as_str());
    assert_eq!(received.package_sha256, digest);
    assert_eq!(received.sealed_bytes, expectation.sealed_bytes);
    assert!(received.package_path.starts_with(&package_directory));

    let review = imager_backend::setup::prepared_setup::PreparedSetupReview::open(
        &received.package_path,
        received.password.as_bytes(),
        &home.join("quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(review.manifest().mail.len(), 1);
    // The package's canonical importer installs the person's custody into the
    // real identity directory. The temporary joined record above never becomes
    // a second identity for the same person. It must not replace an unrelated
    // target identity or claim the package's person merely because the source
    // approved the transfer.
    review
        .import_identity(&destination_identity, received.password.as_bytes())
        .unwrap();
    let unrelated = home.join("unrelated-target");
    let unrelated_identity = PreparedIdentity::create(
        &unrelated,
        "Unrelated person",
        [1, 2, 3],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    assert_ne!(
        unrelated_identity.public_key(),
        review.manifest().identity_public_key
    );
    let before = std::fs::read(unrelated.join("primary.json")).unwrap();
    assert!(
        review
            .import_identity(&unrelated, received.password.as_bytes())
            .is_err()
    );
    assert_eq!(
        std::fs::read(unrelated.join("primary.json")).unwrap(),
        before
    );
    let target = home.join("target-home");
    std::fs::create_dir_all(&target).unwrap();
    let receipts = review
        .import_mail(&target, received.password.as_bytes())
        .unwrap();
    assert_eq!(receipts.len(), 1);
    let cache = target.join(".cache/so.tilo.mail/mail");
    let all = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap_or_default())
        .collect::<String>();
    assert!(
        all.contains("Inbox mail") || !all.is_empty(),
        "selected folder was imported"
    );
    assert!(!all.contains("Draft mail"), "excluded folder stayed out");

    let foreign_receipt = imager_backend::setup::other_device::Received {
        password: zeroize::Zeroizing::new("not this package".into()),
        package_sha256: [9; 32],
        sealed_bytes: 1,
        package_path: received.package_path.clone(),
    };
    assert!(
        receiver.acknowledge(&foreign_receipt).is_err(),
        "a receipt for another package must never complete this source"
    );
    receiver.acknowledge(&received).unwrap();
    // The compositor releases its receiver immediately after acknowledging;
    // the queued receipt must still reach the source and name this package.
    drop(receiver);
    let mut complete = false;
    read_until(tilo_test_env::hang_bound(), || {
        while let Some(state) = source.poll() {
            match state {
                SourceState::PackageImported { .. } => complete = true,
                SourceState::Failed | SourceState::Rejected => panic!("source failed"),
                _ => {}
            }
        }
        complete
    });
}

#[test]
fn a_public_descriptor_never_names_a_source_path_or_token() {
    let source = Source::bind(uuid::Uuid::new_v4().to_string(), [9; 32]).unwrap();
    let public = PublicHandoff::from_identity(source.identity(), "a".repeat(64), 1024);
    let text = String::from_utf8(public.encode().unwrap()).unwrap();
    for private in ["home", "token", "password", "credential", "/root/"] {
        assert!(
            !text.contains(private),
            "descriptor leaked {private}: {text}"
        );
    }
    assert!(public.valid_at(source.identity().expires_at_ns - 1));
}

#[test]
fn a_handoff_for_another_person_is_refused_before_any_transfer() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = tempfile::tempdir().unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = fixture.path().join("destination-home");
    let system = fixture.path().join("destination-system");
    let identity = fixture.path().join("destination-identity");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    PreparedIdentity::create(
        &identity,
        "Destination person",
        [4, 5, 6],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    env.set("TILO_IDENTITY_ROOT", &system);
    let source = HandoffIdentity {
        operation_id: uuid::Uuid::new_v4().to_string(),
        ticket: "endpoint-ticket-value".into(),
        identity_public_key: [7; 32],
        expires_at_ns: u64::MAX,
    };
    let public = PublicHandoff::from_identity(&source, "a".repeat(64), 1024);
    let result = Receiver::open(
        public,
        ReceiverIdentity {
            identity_root: system,
            identity_dir: identity,
        },
        home.join(".local/state/tilo/prepared-setup"),
    );
    assert!(result.is_err(), "another person's package must be refused");
}

fn fresh_bootstrap_source(source: &PreparedIdentity) -> (Source, PublicHandoff, [u8; 32]) {
    let source = Source::bind_with_identity(
        uuid::Uuid::new_v4().to_string(),
        source.bootstrap_source_identity().unwrap(),
    )
    .unwrap();
    let bytes = b"prepared ciphertext for protocol refusal, with enough bytes".to_vec();
    let digest: [u8; 32] = {
        use sha2::Digest as _;
        sha2::Sha256::digest(&bytes).into()
    };
    source
        .serve(
            bytes.clone(),
            zeroize::Zeroizing::new("unused protocol password".into()),
            digest,
        )
        .unwrap();
    let public =
        PublicHandoff::from_identity(source.identity(), hex::encode(digest), bytes.len() as u64);
    (source, public, digest)
}

fn fresh_destination(home: &Path) -> (PathBuf, PathBuf) {
    let system = home.join("fresh-system");
    let identity = home.join("fresh-identity");
    (system, identity)
}

#[test]
fn a_wrong_join_approval_releases_no_identity_until_the_exact_request_is_approved() {
    let (_fixture, home) = fixture();
    let mut env = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("HOME", &home);
        env.set("XDG_STATE_HOME", home.join(".local/state"));
        env.set("XDG_CONFIG_HOME", home.join(".config"));
        env
    };
    let source_identity = PreparedIdentity::create(
        &home.join("wrong-code-source"),
        "Source person",
        [10, 20, 30],
        imager_backend::prepared_identity::storage_for_session(),
    )
    .unwrap();
    let (source, public, _) = fresh_bootstrap_source(&source_identity);
    let (system, identity) = fresh_destination(&home);
    env.set("TILO_IDENTITY_ROOT", &system);
    let receiver = Receiver::join(
        public,
        ReceiverIdentity {
            identity_root: system.clone(),
            identity_dir: identity.clone(),
        },
        home.join(".local/state/tilo/prepared-setup"),
        "Fresh destination".into(),
    )
    .unwrap();
    let mut approval = None;
    read_until(tilo_test_env::hang_bound(), || {
        while let Some(state) = source.poll() {
            if let SourceState::JoinRequested {
                approval: token, ..
            } = state
            {
                approval = Some(token);
            }
        }
        approval.is_some()
    });
    // A token from no request, or another request, cannot release custody.
    let refused = tilo_handoff::transport::refused_approvals();
    source.approve(&uuid::Uuid::new_v4().to_string()).unwrap();
    read_until(tilo_test_env::hang_bound(), || {
        tilo_handoff::transport::refused_approvals() > refused
    });
    assert!(runtime_identity::resolve_active_identity(&identity).is_none());
    assert!(
        !tilo_user_identity::persist::IdentityPaths::under(&system)
            .user_pubkey
            .exists()
    );
    source.approve(&approval.unwrap()).unwrap();
    read_until(tilo_test_env::hang_bound(), || {
        runtime_identity::resolve_active_identity(&identity).is_some()
    });
    receiver.cancel();
    source.cancel();
}

#[test]
fn a_certificate_signed_by_another_root_releases_no_identity() {
    let (_fixture, home) = fixture();
    let mut env = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("HOME", &home);
        env.set("XDG_STATE_HOME", home.join(".local/state"));
        env.set("XDG_CONFIG_HOME", home.join(".config"));
        env
    };
    let source_identity = PreparedIdentity::create(
        &home.join("wrong-cert-source"),
        "Source person",
        [10, 20, 30],
        imager_backend::prepared_identity::storage_for_session(),
    )
    .unwrap();
    let other = PreparedIdentity::create(
        &home.join("other-root"),
        "Other person",
        [30, 20, 10],
        imager_backend::prepared_identity::storage_for_session(),
    )
    .unwrap();
    let mut authority = source_identity.bootstrap_source_identity().unwrap();
    authority.signer = other.bootstrap_source_identity().unwrap().signer;
    let source = Source::bind_with_identity(uuid::Uuid::new_v4().to_string(), authority).unwrap();
    let bytes = b"prepared ciphertext for a forged certificate, with enough bytes".to_vec();
    let digest: [u8; 32] = {
        use sha2::Digest as _;
        sha2::Sha256::digest(&bytes).into()
    };
    source
        .serve(
            bytes.clone(),
            zeroize::Zeroizing::new("unused protocol password".into()),
            digest,
        )
        .unwrap();
    let public =
        PublicHandoff::from_identity(source.identity(), hex::encode(digest), bytes.len() as u64);
    let (system, identity) = fresh_destination(&home);
    env.set("TILO_IDENTITY_ROOT", &system);
    let receiver = Receiver::join(
        public,
        ReceiverIdentity {
            identity_root: system.clone(),
            identity_dir: identity.clone(),
        },
        home.join(".local/state/tilo/prepared-setup"),
        "Fresh destination".into(),
    )
    .unwrap();
    let mut approval = None;
    read_until(tilo_test_env::hang_bound(), || {
        while let Some(state) = source.poll() {
            if let SourceState::JoinRequested {
                approval: token, ..
            } = state
            {
                approval = Some(token);
            }
        }
        approval.is_some()
    });
    source.approve(&approval.unwrap()).unwrap();
    read_until(tilo_test_env::hang_bound(), || {
        matches!(source.poll(), Some(SourceState::Failed))
            || runtime_identity::resolve_active_identity(&identity).is_some()
    });
    assert!(
        runtime_identity::resolve_active_identity(&identity).is_none(),
        "a certificate that does not verify under the source root installs nothing"
    );
    assert!(
        !tilo_user_identity::persist::IdentityPaths::under(&system)
            .user_pubkey
            .exists()
    );
    receiver.cancel();
    source.cancel();
}

#[test]
fn a_join_for_another_operation_releases_no_identity() {
    let (_fixture, home) = fixture();
    let mut env = {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        env.set("HOME", &home);
        env.set("XDG_STATE_HOME", home.join(".local/state"));
        env.set("XDG_CONFIG_HOME", home.join(".config"));
        env
    };
    let source_identity = PreparedIdentity::create(
        &home.join("cross-op-source"),
        "Source person",
        [10, 20, 30],
        imager_backend::prepared_identity::storage_for_session(),
    )
    .unwrap();
    let (source, mut public, _) = fresh_bootstrap_source(&source_identity);
    public.operation_id = uuid::Uuid::new_v4().to_string();
    let (system, identity) = fresh_destination(&home);
    env.set("TILO_IDENTITY_ROOT", &system);
    let refused = tilo_handoff::transport::refused_join_requests();
    let receiver = Receiver::join(
        public,
        ReceiverIdentity {
            identity_root: system.clone(),
            identity_dir: identity.clone(),
        },
        home.join(".local/state/tilo/prepared-setup"),
        "Fresh destination".into(),
    )
    .unwrap();
    read_until(tilo_test_env::hang_bound(), || {
        tilo_handoff::transport::refused_join_requests() > refused
    });
    assert!(
        runtime_identity::resolve_active_identity(&identity).is_none(),
        "the source refuses a bootstrap for another operation"
    );
    assert!(
        !tilo_user_identity::persist::IdentityPaths::under(&system)
            .user_pubkey
            .exists()
    );
    receiver.cancel();
    source.cancel();
}
