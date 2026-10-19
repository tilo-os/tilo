use super::*;
use crate::setup::{
    portable_home_media::{HomeSource, MediaGrant},
    portable_transfer::{ExportReview, Limits},
    prepared_identity::PreparedIdentity,
    prepared_setup::{Manifest, PortableHomePlan, PortableHomeSelection},
};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::{Duration, Instant},
};
use tilo_identity_host::provisioning::IdentityStorage;
const PASSWORD: &[u8] = b"several unrelated installation words";
const BINDING: &str = "chosen-setup-operation";
fn private(path: &std::path::Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
struct Fixture {
    _environment: tilo_test_env::ScopedEnv,
    _temporary: tempfile::TempDir,
    source: PathBuf,
    setup: PreparedSetupReview,
    grants: HomeImportGrants,
    expected: Expectation,
}
fn prepared_setup(root: &std::path::Path) -> PreparedSetupReview {
    let staging = root.join("setup-staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(crate::setup::prepared_setup::IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let mut manifest = Manifest::new(BINDING.into(), identity.public_key(), vec![]).unwrap();
    manifest.portable_home = Some(PortableHomePlan {
        schema: 1,
        selection: PortableHomeSelection::FullHome,
        applications: false,
    });
    manifest.write(&staging).unwrap();
    let setup_package = root.join("setup.sealed");
    ExportReview::review(
        &staging,
        &setup_package,
        Purpose::PreparedSetup,
        Limits {
            plaintext_bytes: 16 * 1024 * 1024,
            entries: 2048,
            metadata_bytes: 2 * 1024 * 1024,
        },
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    PreparedSetupReview::open(
        &setup_package,
        PASSWORD,
        &root.join("setup-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn fixture() -> Fixture {
    fixture_with_root_acl(false)
}
fn fixture_with_root_acl(root_acl: bool) -> Fixture {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    private(&root);
    let source = root.join("source");
    private(&source);
    private(&source.join(".config/tilo"));
    private(&source.join(".local/share/flatpak/repo"));
    private(&source.join(".var/app/org.example.App"));
    fs::write(source.join("document"), b"original document").unwrap();
    fs::write(
        source.join(".config/tilo/identity"),
        b"old identity custody",
    )
    .unwrap();
    fs::write(
        source.join(".local/share/flatpak/repo/user.trustedkeys.gpg"),
        b"original trust keys",
    )
    .unwrap();
    fs::write(
        source.join(".var/app/org.example.App/session"),
        b"private session",
    )
    .unwrap();
    fs::set_permissions(source.join("document"), fs::Permissions::from_mode(0o750)).unwrap();
    symlink("document", source.join("shortcut")).unwrap();
    let setup = prepared_setup(&root);
    let media = root.join("media");
    private(&media);
    if root_acl {
        crate::portable_transfer::acl::fixture::modify(
            &source,
            "u::rwx,g::r-x,m::---,o::---,d:u::rwx,d:g::r-x,d:m::r-x,d:o::---",
        );
    }
    let expected = HomeSource::review(&source)
        .unwrap()
        .export_to(
            &MediaGrant::open(&media).unwrap(),
            "home-operation",
            BINDING,
            Zeroizing::new(PASSWORD.to_vec()),
            &AtomicBool::new(false),
        )
        .unwrap();
    let home = root.join("active-home");
    private(&home.join(".config/tilo"));
    private(&home.join(".local/state/tilo"));
    private(&home);
    fs::write(
        home.join(".config/tilo/identity"),
        b"chosen new identity custody",
    )
    .unwrap();
    let workspace = home.join(".local/state/tilo/portable-home/operation");
    private(&workspace);
    let grants = HomeImportGrants {
        package: media.join(&expected.filename),
        home,
        workspace,
    };
    Fixture {
        _environment: environment,
        _temporary: temporary,
        source,
        setup,
        grants,
        expected,
    }
}
fn finished(worker: &mut AdoptionWorker) -> AdoptionResult {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = worker.try_next() {
            return result.unwrap();
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn authenticated_home_adopts_files_flatpak_data_and_links_without_old_identity_takeover() {
    let fixture = fixture();
    let cancel = AtomicBool::new(false);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    let preview = review
        .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert!(preview.install_items > 0);
    assert!(preview.inactive_custody > 0);
    assert!(!fixture.grants.home.join("document").exists());
    assert!(review.verify_completed_adoption(&cancel).is_err());
    assert!(finished(&mut review.start_adoption(&cancel).unwrap()).complete);
    review.verify_completed_adoption(&cancel).unwrap();
    assert_eq!(
        fs::read(fixture.grants.home.join("document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        fs::read(fixture.grants.home.join(".config/tilo/identity")).unwrap(),
        b"chosen new identity custody"
    );
    assert_eq!(
        fs::read(fixture.grants.home.join(".var/app/org.example.App/session")).unwrap(),
        b"private session"
    );
    assert_eq!(
        fs::read(
            fixture
                .grants
                .home
                .join(".local/share/flatpak/repo/user.trustedkeys.gpg")
        )
        .unwrap(),
        b"original trust keys"
    );
    assert_eq!(
        fs::read_link(fixture.grants.home.join("shortcut")).unwrap(),
        PathBuf::from("document")
    );
    assert_eq!(
        fs::metadata(fixture.grants.home.join("document"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        fs::read(fixture.source.join("document")).unwrap(),
        b"original document"
    );
}
#[test]
fn portable_home_restart_reauthenticates_actual_moved_and_retained_archive_records() {
    let fixture = fixture();
    let cancel = AtomicBool::new(false);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    review
        .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    review
        .promotion
        .as_ref()
        .unwrap()
        .move_one_for_test(std::path::Path::new("document"))
        .unwrap();
    drop(review);
    let mut resumed = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    assert!(resumed.verify_completed_adoption(&cancel).is_err());
    assert!(
        resumed
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .unwrap()
            .recovered
    );
    assert!(finished(&mut resumed.start_adoption(&cancel).unwrap()).complete);
    resumed.verify_completed_adoption(&cancel).unwrap();
    assert_eq!(
        fs::read(fixture.grants.home.join("document")).unwrap(),
        b"original document"
    );
    fs::write(
        fixture.grants.home.join("document"),
        b"changed user document",
    )
    .unwrap();
    let mut altered = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    assert!(
        altered
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.grants.home.join("document")).unwrap(),
        b"changed user document"
    );
}
#[test]
fn mutable_media_expectation_cannot_replace_authenticated_setup_binding() {
    let fixture = fixture();
    let mut expected = fixture.expected.clone();
    expected.setup_binding = "another-setup".into();
    assert!(
        PortableHomeReview::review(
            fixture.grants.clone(),
            &fixture.setup,
            &expected,
            PASSWORD,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!fixture.grants.workspace.join("imported").exists());
    let mut expected = fixture.expected.clone();
    *expected.ciphertext_sha256.first_mut().unwrap() ^= 1;
    assert!(
        PortableHomeReview::review(
            fixture.grants.clone(),
            &fixture.setup,
            &expected,
            PASSWORD,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!fixture.grants.workspace.join("imported").exists());
    assert_eq!(
        fs::read(fixture.grants.home.join(".config/tilo/identity")).unwrap(),
        b"chosen new identity custody"
    );
}

fn helper_args(fixture: &Fixture, action: &str, setup_quarantine: &str) -> Vec<std::ffi::OsString> {
    let root = fixture._temporary.path().canonicalize().unwrap();
    let receipt = fixture.grants.workspace.join("receipt.json");
    fs::write(&receipt, serde_json::to_vec(&fixture.expected).unwrap()).unwrap();
    fs::set_permissions(&receipt, fs::Permissions::from_mode(0o600)).unwrap();
    [
        ("--package", fixture.grants.package.clone().into_os_string()),
        ("--receipt", receipt.into_os_string()),
        (
            "--setup-package",
            root.join("setup.sealed").into_os_string(),
        ),
        (
            "--setup-quarantine",
            root.join(setup_quarantine).into_os_string(),
        ),
        ("--home", fixture.grants.home.clone().into_os_string()),
        (
            "--workspace",
            fixture.grants.workspace.clone().into_os_string(),
        ),
        ("--action", action.into()),
    ]
    .into_iter()
    .flat_map(|(name, value)| [name.into(), value])
    .collect()
}
#[test]
fn native_helper_reviews_then_explicitly_adopts_same_authenticated_home_controller() {
    let fixture = fixture();
    let mut output = Vec::new();
    let mut secret = PASSWORD;
    crate::setup::portable_home_helper::run_with_admission(
        helper_args(&fixture, "review", "helper-review-setup"),
        &mut secret,
        &mut output,
        || {
            crate::setup::home_admission::require(
                Some(&crate::setup::test_support::home_consumer()),
                Some(2),
            )
        },
    )
    .unwrap();
    let review: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(review["event"], "ready-to-adopt");
    assert!(!fixture.grants.home.join("document").exists());
    output.clear();
    secret = PASSWORD;
    crate::setup::portable_home_helper::run_with_admission(
        helper_args(&fixture, "adopt", "helper-adopt-setup"),
        &mut secret,
        &mut output,
        || {
            crate::setup::home_admission::require(
                Some(&crate::setup::test_support::home_consumer()),
                Some(2),
            )
        },
    )
    .unwrap();
    let events = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.last().unwrap()["event"], "complete");
    assert_eq!(
        fs::read(fixture.grants.home.join("document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        fs::read(fixture.grants.home.join(".config/tilo/identity")).unwrap(),
        b"chosen new identity custody"
    );
    assert_eq!(
        fs::read(fixture.source.join("document")).unwrap(),
        b"original document"
    );
}

#[test]
fn authenticated_footer_binding_refuses_another_home_even_with_rewritten_media_receipt() {
    let fixture = fixture();
    let media = fixture.grants.package.parent().unwrap();
    let mut other = HomeSource::review(&fixture.source)
        .unwrap()
        .export_to(
            &MediaGrant::open(media).unwrap(),
            "other-home-operation",
            "another-authenticated-setup",
            Zeroizing::new(PASSWORD.to_vec()),
            &AtomicBool::new(false),
        )
        .unwrap();
    other.setup_binding = BINDING.into();
    let mut grants = fixture.grants.clone();
    grants.package = media.join(&other.filename);
    let error = PortableHomeReview::review(
        grants,
        &fixture.setup,
        &other,
        PASSWORD,
        &AtomicBool::new(false),
    )
    .err()
    .unwrap();
    assert_eq!(
        error.to_string(),
        "Portable home ciphertext or authenticated declaration changed"
    );
    assert!(!fixture.grants.workspace.join("imported").exists());
    assert!(!fixture.grants.home.join("document").exists());
}

#[test]
fn portable_adoption_keeps_existing_documents_and_reports_collisions_incomplete() {
    let fixture = fixture();
    fs::write(
        fixture.grants.home.join("document"),
        b"existing destination document",
    )
    .unwrap();
    let cancel = AtomicBool::new(false);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    assert!(
        review
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .unwrap()
            .collisions
            > 0
    );
    let result = finished(&mut review.start_adoption(&cancel).unwrap());
    assert!(!result.complete);
    assert!(result.collisions > 0);
    assert_eq!(
        fs::read(fixture.grants.home.join("document")).unwrap(),
        b"existing destination document"
    );
    assert_eq!(
        fs::read(fixture.grants.workspace.join("imported/home/document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        fs::read(fixture.source.join("document")).unwrap(),
        b"original document"
    );
}

#[test]
fn prepared_setup_helper_preserves_authenticated_home_declaration_for_first_login() {
    let fixture = fixture();
    let root = fixture._temporary.path().canonicalize().unwrap();
    let args = [
        ("--package", root.join("setup.sealed")),
        ("--quarantine", root.join("helper-custody-quarantine")),
        ("--identity-dir", root.join("helper-identities")),
        ("--config-root", root.join("helper-accounts")),
    ]
    .into_iter()
    .flat_map(|(name, path)| [std::ffi::OsString::from(name), path.into_os_string()]);
    let mut output = Vec::new();
    let mut secret = PASSWORD;
    crate::setup::prepared_setup_helper::run_with_home_admission(
        args,
        &mut secret,
        &mut output,
        || {
            crate::setup::home_admission::require(
                Some(&crate::setup::test_support::home_consumer()),
                Some(2),
            )
        },
    )
    .unwrap();
    let receipt: crate::setup::prepared_setup_helper::Receipt =
        serde_json::from_slice(&output).unwrap();
    let declaration = receipt.portable_home.unwrap();
    assert_eq!(declaration.schema, 1);
    assert_eq!(declaration.setup_binding, BINDING);
    assert_eq!(declaration.selection, PortableHomeSelection::FullHome);
    assert!(!fixture.grants.home.join("document").exists());
}

#[test]
fn native_home_consumer_refuses_missing_protocol_before_any_extraction_or_adoption() {
    let fixture = fixture();
    let setup_quarantine = fixture._temporary.path().join("refused-consumer-setup");
    let original = fs::read(&fixture.grants.package).unwrap();
    let mut output = Vec::new();
    let mut secret = PASSWORD;
    let result = crate::setup::portable_home_helper::run_with_admission(
        helper_args(&fixture, "adopt", "refused-consumer-setup"),
        &mut secret,
        &mut output,
        || crate::setup::home_admission::require(None, Some(2)),
    );
    assert!(result.is_err());
    assert!(output.is_empty());
    assert!(!setup_quarantine.exists());
    assert!(!fixture.grants.home.join("document").exists());
    assert_eq!(fs::read(&fixture.grants.package).unwrap(), original);
}

#[test]
fn portable_home_root_acl_reaches_active_home_and_restart_verifies_it() {
    let fixture = fixture_with_root_acl(true);
    let cancel = AtomicBool::new(false);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    review
        .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    review
        .promotion
        .as_ref()
        .unwrap()
        .apply_root_acl_before_checkpoint_for_test()
        .unwrap();
    assert!(review.verify_completed_adoption(&cancel).is_err());
    drop(review);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    assert!(
        review
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .unwrap()
            .recovered
    );
    assert!(finished(&mut review.start_adoption(&cancel).unwrap()).complete);
    review.verify_completed_adoption(&cancel).unwrap();
    let get = crate::portable_transfer::acl::fixture::state;
    assert_eq!(get(&fixture.source), get(&fixture.grants.home));
    assert_eq!(
        fs::metadata(&fixture.grants.home)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    drop(review);
    let mut resumed = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    resumed.verify_completed_adoption(&cancel).unwrap();
    assert!(
        resumed
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .unwrap()
            .recovered
    );
    fs::write(
        fixture.grants.home.join("created-after-adoption"),
        b"new document",
    )
    .unwrap();
    assert_eq!(
        fs::metadata(fixture.grants.home.join("created-after-adoption"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    fs::remove_file(fixture.grants.home.join("created-after-adoption")).unwrap();
    resumed.verify_completed_adoption(&cancel).unwrap();
    crate::portable_transfer::acl::fixture::modify(&fixture.grants.home, "d:m::---");
    assert!(resumed.verify_completed_adoption(&cancel).is_err());
}
#[test]
fn portable_home_refuses_to_replace_existing_root_default_acl() {
    let fixture = fixture_with_root_acl(true);
    crate::portable_transfer::acl::fixture::modify(
        &fixture.grants.home,
        "d:u::rwx,d:g::---,d:m::---,d:o::---",
    );
    let before = crate::portable_transfer::acl::fixture::state(&fixture.grants.home);
    let cancel = AtomicBool::new(false);
    let mut review = PortableHomeReview::review(
        fixture.grants.clone(),
        &fixture.setup,
        &fixture.expected,
        PASSWORD,
        &cancel,
    )
    .unwrap();
    assert!(
        review
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .is_err()
    );
    assert_eq!(
        before,
        crate::portable_transfer::acl::fixture::state(&fixture.grants.home)
    );
    assert!(!fixture.grants.home.join("document").exists());
    assert_eq!(
        fs::read(fixture.source.join("document")).unwrap(),
        b"original document"
    );
}

#[test]
fn authenticated_completed_setup_binds_exact_home_before_adoption() {
    let f = fixture();
    let root = f.source.parent().unwrap();
    let staging = root.join("setup-staging");
    let mut manifest = f.setup.manifest().clone();
    manifest.completed_home = Some(f.expected.clone());
    fs::remove_file(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap();
    manifest.write(&staging).unwrap();
    let package = root.join("completed-setup.sealed");
    ExportReview::review(
        &staging,
        &package,
        Purpose::PreparedSetup,
        crate::setup::prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    let completed = PreparedSetupReview::open(
        &package,
        PASSWORD,
        &root.join("completed-review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    completed.require_completed_export().unwrap();
    assert_eq!(
        completed.manifest().completed_home.as_ref(),
        Some(&f.expected)
    );
    // The canonical installer authentication helper must carry this exact
    // password-authenticated archive identity before destructive installation.
    use sha2::Digest as _;
    let bytes = fs::read(&package).unwrap();
    let receipt = root.join("completed-auth-receipt.json");
    fs::write(
        &receipt,
        serde_json::to_vec(&serde_json::json!({
            "schema": 1, "sealed_bytes": bytes.len(),
            "ciphertext_sha256": hex::encode(sha2::Sha256::digest(&bytes))
        }))
        .unwrap(),
    )
    .unwrap();
    let args = [
        ("--package", package),
        ("--receipt", receipt),
        ("--quarantine", root.join("completed-installer-auth")),
    ]
    .into_iter()
    .flat_map(|(key, path)| [std::ffi::OsString::from(key), path.into_os_string()]);
    let mut public = Vec::new();
    crate::setup::prepared_setup_helper::authenticate::run(args, &mut &PASSWORD[..], &mut public)
        .unwrap();
    let public: serde_json::Value = serde_json::from_slice(&public).unwrap();
    assert_eq!(
        public["completed_home"],
        serde_json::to_value(&f.expected).unwrap()
    );
    assert_eq!(
        public["completed_setup"],
        serde_json::json!({
            "schema":1, "sealed_bytes":bytes.len(),
            "ciphertext_sha256":hex::encode(sha2::Sha256::digest(&bytes))
        })
    );
    assert_eq!(public["setup_binding"], completed.manifest().setup_binding);

    assert!(
        PortableHomeReview::review(
            f.grants.clone(),
            &completed,
            &f.expected,
            PASSWORD,
            &AtomicBool::new(false)
        )
        .is_ok()
    );
    let mut other_manifest = manifest;
    other_manifest
        .completed_home
        .as_mut()
        .unwrap()
        .ciphertext_sha256[0] ^= 1;
    fs::remove_file(staging.join(crate::setup::prepared_setup::MANIFEST_FILE)).unwrap();
    other_manifest.write(&staging).unwrap();
    let other_package = root.join("different-completed-setup.sealed");
    ExportReview::review(
        &staging,
        &other_package,
        Purpose::PreparedSetup,
        crate::setup::prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    let other = PreparedSetupReview::open(
        &other_package,
        PASSWORD,
        &root.join("different-completed-review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    // The archive is unchanged and authenticated, but this different sealed
    // setup authorizes a different Home expectation. No adoption can begin.
    assert!(
        PortableHomeReview::review(
            f.grants.clone(),
            &other,
            &f.expected,
            PASSWORD,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(
        fs::read(f.source.join("document")).unwrap(),
        b"original document"
    );
    assert!(!f.grants.workspace.join("adoption.journal").exists());
}
