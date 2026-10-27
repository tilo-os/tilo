use super::*;
use crate::setup::portable_transfer::ImportReview;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::PathBuf;
const PASSWORD: &[u8] = b"portable home fixture secret";
#[test]
fn setup_selection_pairs_completed_ciphertext_without_overwriting_another_transfer() {
    let root = fixture();
    let media = root.path().join("media");
    let source = root.path().join("source");
    std::fs::write(source.join("document"), b"my document").unwrap();
    let grant = MediaGrant::open(&media).unwrap();
    let cancel = AtomicBool::new(false);
    let home = HomeSource::review(&source)
        .unwrap()
        .export_to(
            &grant,
            "selected",
            "prepared-binding",
            Zeroizing::new(PASSWORD.to_vec()),
            &cancel,
        )
        .unwrap();
    grant.publish_expectation(&home, &cancel).unwrap();
    std::fs::create_dir(media.join(super::super::media_setup::DIRECTORY)).unwrap();
    // This receipt locates opaque setup bytes; the native consumer separately
    // authenticates identity custody. Publication must not imply that authority.
    let setup = super::super::media_setup::DeliveryReceipt {
        relative_path: PathBuf::from(".tilo-installer/prepared-setup.sealed"),
        sealed_bytes: 48,
        ciphertext_sha256: "a".repeat(64),
    };
    grant
        .publish_setup_selection(&setup, &home, &cancel)
        .unwrap();
    let path = media.join(".tilo-installer/home-selection.json");
    let before = std::fs::read(&path).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(value["home"]["filename"], home.filename);
    assert_eq!(value["home"]["setup_binding"], "prepared-binding");
    grant
        .publish_setup_selection(&setup, &home, &cancel)
        .unwrap();
    let mut other = setup;
    other.ciphertext_sha256 = "b".repeat(64);
    assert!(
        grant
            .publish_setup_selection(&other, &home, &cancel)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        std::fs::read(source.join("document")).unwrap(),
        b"my document"
    );
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for name in ["source", "media", "target"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
        std::fs::set_permissions(
            root.path().join(name),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    root
}
#[test]
fn full_home_streams_directly_to_granted_media_and_authenticated_binding_roundtrips() {
    let root = fixture();
    let source = root.path().join("source");
    std::fs::create_dir_all(source.join(".var/app/example/data")).unwrap();
    std::fs::write(
        source.join(".var/app/example/data/session"),
        b"private application data",
    )
    .unwrap();
    std::fs::write(source.join("script"), b"executable user file").unwrap();
    std::fs::set_permissions(
        source.join("script"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    std::fs::hard_link(source.join("script"), source.join("alias")).unwrap();
    symlink("/original/external/path", source.join("external")).unwrap();
    let reviewed = HomeSource::review(&source).unwrap();
    let grant = MediaGrant::open(&root.path().join("media")).unwrap();
    let cancel = AtomicBool::new(false);
    let mut exported = Vec::new();
    let receipt = reviewed
        .export_to_reporting(
            &grant,
            "operation",
            "authenticated-preparation-binding",
            Zeroizing::new(PASSWORD.to_vec()),
            &cancel,
            &mut |progress| {
                exported.push(progress);
                Ok(())
            },
        )
        .unwrap();
    assert_home_progress(&exported, receipt.plaintext_bytes);
    receipt.validate().unwrap();
    assert_eq!(receipt.filename, "home-operation.sealed");
    let media_entries = std::fs::read_dir(root.path().join("media"))
        .unwrap()
        .count();
    assert_eq!(
        media_entries, 1,
        "Only final encrypted package should remain"
    );
    let mut verification = Vec::new();
    reviewed
        .validate_existing_reporting(
            &grant,
            &receipt,
            "authenticated-preparation-binding",
            PASSWORD,
            &cancel,
            &mut |progress| {
                verification.push(progress);
                Ok(())
            },
        )
        .unwrap();
    assert!(
        verification
            .iter()
            .all(|p| p.phase != portable_transfer::ArchivePhase::Archiving)
    );
    assert_home_progress(&verification, receipt.plaintext_bytes);
    assert!(
        reviewed
            .validate_existing(&grant, &receipt, "another-preparation", PASSWORD, &cancel)
            .is_err()
    );
    grant.publish_expectation(&receipt, &cancel).unwrap();
    grant.publish_expectation(&receipt, &cancel).unwrap();
    let mut changed_receipt = receipt.clone();
    changed_receipt.setup_binding = "another-preparation".into();
    assert!(
        grant
            .publish_expectation(&changed_receipt, &cancel)
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("media"))
            .unwrap()
            .count(),
        2
    );
    let mut file = File::open(root.path().join("media").join(&receipt.filename)).unwrap();
    assert_eq!(
        ciphertext_digest(&mut file, &cancel).unwrap(),
        receipt.ciphertext_sha256
    );
    let package = root.path().join("media").join(&receipt.filename);
    let review =
        ImportReview::review(&package, PASSWORD, Purpose::Home, receipt.limits, &cancel).unwrap();
    assert_eq!(
        review.info().setup_binding.as_deref(),
        Some("authenticated-preparation-binding")
    );
    let imported = review
        .extract(
            Zeroizing::new(PASSWORD.to_vec()),
            &root.path().join("target/home"),
            &cancel,
        )
        .unwrap();
    assert_eq!(
        std::fs::read(imported.root.join(".var/app/example/data/session")).unwrap(),
        b"private application data"
    );
    assert_eq!(
        std::fs::metadata(imported.root.join("script"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::read_link(imported.root.join("external")).unwrap(),
        Path::new("/original/external/path")
    );
    assert_eq!(
        std::fs::read(source.join("script")).unwrap(),
        b"executable user file"
    );
}
#[test]
fn cancellation_source_mutation_and_existing_publication_never_replace_or_publish() {
    let root = fixture();
    let source = root.path().join("source");
    std::fs::write(source.join("document"), b"original").unwrap();
    let review = HomeSource::review(&source).unwrap();
    let grant = MediaGrant::open(&root.path().join("media")).unwrap();
    assert!(
        review
            .export_to(
                &grant,
                "cancelled",
                "binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &AtomicBool::new(true)
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("media"))
            .unwrap()
            .count(),
        0
    );
    std::fs::write(source.join("document"), b"changed after consent").unwrap();
    assert!(
        review
            .export_to(
                &grant,
                "changed",
                "binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("media"))
            .unwrap()
            .count(),
        0
    );
    let fresh = HomeSource::review(&source).unwrap();
    let target = root.path().join("media/home-existing.sealed");
    std::fs::write(&target, b"existing package").unwrap();
    assert!(
        fresh
            .export_to(
                &grant,
                "existing",
                "binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(std::fs::read(target).unwrap(), b"existing package");
    assert_eq!(
        std::fs::read_dir(root.path().join("media"))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn changed_media_grant_and_source_descendant_destination_are_refused() {
    let root = fixture();
    let source = root.path().join("source");
    std::fs::write(source.join("document"), b"unchanged source").unwrap();
    std::fs::create_dir(source.join("nested")).unwrap();
    let reviewed = HomeSource::review(&source).unwrap();
    let nested = MediaGrant::open(&source.join("nested")).unwrap();
    assert!(
        reviewed
            .export_to(
                &nested,
                "recursive",
                "binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    let grant = MediaGrant::open(&root.path().join("media")).unwrap();
    std::fs::rename(root.path().join("media"), root.path().join("old-media")).unwrap();
    std::fs::create_dir(root.path().join("media")).unwrap();
    assert!(
        reviewed
            .export_to(
                &grant,
                "replaced",
                "binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("old-media"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("media"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn cancellation_after_ciphertext_creation_cleans_partial_without_publication() {
    let root = fixture();
    let source = root.path().join("source");
    std::fs::write(source.join("document"), vec![0x5a; 4 * 1024 * 1024]).unwrap();
    let reviewed = HomeSource::review(&source).unwrap();
    let media_path = root.path().join("media");
    let grant = MediaGrant::open(&media_path).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);
    // The export is held at its first progress report, with its ciphertext
    // storage created, until the test has cancelled it.
    let (reached, observed) = std::sync::mpsc::channel();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let worker = std::thread::spawn(move || {
        let mut hold = Some((reached, held));
        reviewed.export_to_reporting(
            &grant,
            "interrupted",
            "binding",
            Zeroizing::new(PASSWORD.to_vec()),
            &worker_cancel,
            &mut |_| {
                if let Some((reached, held)) = hold.take() {
                    let _ = reached.send(());
                    let _ = held.recv();
                }
                Ok(())
            },
        )
    });
    tilo_test_env::recv("the export to stream into storage", &observed);
    assert!(
        std::fs::read_dir(&media_path).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".partial")
        }),
        "the streaming export has its ciphertext storage"
    );
    cancel.store(true, Ordering::Relaxed);
    release.send(()).unwrap();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(std::fs::read_dir(&media_path).unwrap().count(), 0);
    assert_eq!(
        std::fs::metadata(source.join("document")).unwrap().len(),
        4 * 1024 * 1024
    );
}

#[test]
fn installer_custody_exclusion_is_exact_and_held_namespace_replacement_refuses() {
    let root = fixture();
    let source = root.path().join("source");
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("HOME", &source);
    env.set("XDG_STATE_HOME", source.join(".local/state"));
    let custody = crate::session::preparation_namespace().unwrap();
    std::fs::create_dir_all(&custody).unwrap();
    std::fs::set_permissions(&custody, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(custody.join("draft.json"), b"installer private custody").unwrap();
    let sibling = custody.with_file_name("preparations-user");
    std::fs::create_dir(&sibling).unwrap();
    std::fs::write(sibling.join("notes"), b"real user data beside custody").unwrap();
    let reviewed = HomeSource::review(&source).unwrap();
    std::fs::write(custody.join("new-checkpoint"), b"new installer checkpoint").unwrap();
    reviewed.validate().unwrap();
    let cancel = AtomicBool::new(false);
    let expected = reviewed
        .export_to(
            &MediaGrant::open(&root.path().join("media")).unwrap(),
            "exclusion",
            "binding",
            Zeroizing::new(PASSWORD.to_vec()),
            &cancel,
        )
        .unwrap();
    let imported = ImportReview::review(
        &root.path().join("media").join(&expected.filename),
        PASSWORD,
        Purpose::Home,
        expected.limits,
        &cancel,
    )
    .unwrap()
    .extract(
        Zeroizing::new(PASSWORD.to_vec()),
        &root.path().join("excluded-import"),
        &cancel,
    )
    .unwrap();
    assert!(
        !imported
            .root
            .join(".local/state/tilo/installer/preparations")
            .exists()
    );
    assert_eq!(
        std::fs::read(
            imported
                .root
                .join(".local/state/tilo/installer/preparations-user/notes")
        )
        .unwrap(),
        b"real user data beside custody"
    );
    std::fs::rename(&custody, custody.with_file_name("retained-custody")).unwrap();
    std::fs::create_dir(&custody).unwrap();
    std::fs::set_permissions(&custody, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(reviewed.validate().is_err());
    std::fs::remove_dir(&custody).unwrap();
    symlink(custody.with_file_name("retained-custody"), &custody).unwrap();
    assert!(HomeSource::review(&source).is_err());
    assert_eq!(
        std::fs::read(
            custody
                .with_file_name("retained-custody")
                .join("draft.json")
        )
        .unwrap(),
        b"installer private custody"
    );
}

fn assert_home_progress(progress: &[portable_transfer::ArchiveProgress], plaintext_bytes: u64) {
    use portable_transfer::ArchivePhase;
    let verification: Vec<_> = progress
        .iter()
        .filter(|p| p.phase == ArchivePhase::VerifyingArchive)
        .collect();
    assert_eq!(verification.first().unwrap().completed, 0);
    assert_eq!(verification.last().unwrap().completed, plaintext_bytes);
    assert!(
        verification
            .windows(2)
            .all(|pair| pair[0].completed <= pair[1].completed)
    );
    assert!(progress.iter().all(|p| p.total.is_none()));
    assert_eq!(
        progress.last().unwrap().phase,
        ArchivePhase::VerifyingSource
    );
    let public = serde_json::to_string(progress).unwrap();
    assert!(!public.contains("session"));
    assert!(!public.contains(std::str::from_utf8(PASSWORD).unwrap()));
}
