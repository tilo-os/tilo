//! Genuine applications-only transaction and receiving consumer: no Home substitute.
use super::*;
use crate::setup::{
    flatpak_transfer::{
        Runner as _,
        portable::{ApplicationImport, NativeOfflineRunner},
    },
    home_capture::offline_output::NativeMediumOutput,
    prepared_setup::completion::{self, boot_review::CompletedBootReview},
};
use sha2::Digest as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::process::{Command, Stdio};
const SELECTED_DATABASE: &str = ".var/app/org.tilo.Fixture/data/session.sqlite";
const SELECTED_OVERRIDE: &str = ".local/share/flatpak/overrides/org.tilo.Fixture";
const EXCLUDED_DATA: &str = ".var/app/org.tilo.Excluded/data/document";
const EXCLUDED_OVERRIDE: &str = ".local/share/flatpak/overrides/org.tilo.Excluded";
const STATE_PATHS: [&str; 5] = [
    ".var/app/org.tilo.Fixture/data/selected-document",
    SELECTED_DATABASE,
    SELECTED_OVERRIDE,
    EXCLUDED_DATA,
    EXCLUDED_OVERRIDE,
];
/// Original disposable source only: no selected state is prepared at the target.
pub(super) fn selected_source_state(home: &Path) {
    let database = rusqlite::Connection::open(home.join(SELECTED_DATABASE)).unwrap();
    database
        .execute_batch(
            "CREATE TABLE session (workspace TEXT NOT NULL, revision INTEGER NOT NULL);
             INSERT INTO session VALUES ('original selected workspace', 17);",
        )
        .unwrap();
    database.close().unwrap();
    let excluded = home.join(EXCLUDED_DATA);
    fs::create_dir_all(excluded.parent().unwrap()).unwrap();
    fs::write(excluded, b"excluded original application bytes").unwrap();
    let overrides = home.join(".local/share/flatpak/overrides");
    fs::create_dir_all(&overrides).unwrap();
    fs::write(
        home.join(SELECTED_OVERRIDE),
        b"[Environment]\nTILO_SELECTED_FIXTURE=original-setting\n",
    )
    .unwrap();
    fs::write(
        home.join(EXCLUDED_OVERRIDE),
        b"[Environment]\nTILO_EXCLUDED_FIXTURE=excluded-setting\n",
    )
    .unwrap();
}
fn source_state(original: &File) -> Vec<(String, Vec<u8>)> {
    STATE_PATHS
        .iter()
        .map(|name| ((*name).to_owned(), read_member(original, name)))
        .collect()
}
fn verify_selected_state(home: &Path, root: &Path) {
    let expected: Vec<(String, [u8; 32])> =
        serde_json::from_slice(&fs::read(root.join("source-state-sha256.json")).unwrap()).unwrap();
    assert_eq!(expected.len(), STATE_PATHS.len());
    for ((name, hash), exact) in expected.iter().zip(STATE_PATHS) {
        assert_eq!(name, exact, "source witness names must remain exact");
        if matches!(exact, EXCLUDED_DATA | EXCLUDED_OVERRIDE) {
            assert!(
                !home.join(name).exists(),
                "unselected app state was adopted"
            );
        } else {
            assert_eq!(
                <[u8; 32]>::from(sha2::Sha256::digest(fs::read(home.join(name)).unwrap())),
                *hash,
                "restored selected state differs from actual original bytes: {name}"
            );
        }
    }
    assert!(!home.join(".var/app/org.tilo.Excluded").exists());
    let native_override = NativeOfflineRunner
        .run(
            home,
            &[
                "override".into(),
                "--user".into(),
                "--show".into(),
                "org.tilo.Fixture".into(),
            ],
        )
        .unwrap();
    assert_eq!(
        native_override
            .lines()
            .filter(|line| *line == "TILO_SELECTED_FIXTURE=original-setting")
            .count(),
        1,
        "real Flatpak must recognize the transferred per-app setting"
    );
    let database = rusqlite::Connection::open_with_flags(
        home.join(SELECTED_DATABASE),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let actual: (String, i64) = database
        .query_row("SELECT workspace, revision FROM session", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .unwrap();
    assert_eq!(actual, ("original selected workspace".into(), 17));
    assert_eq!(
        database
            .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
}
pub(super) struct Subjects<'a> {
    pub(super) target: &'a crate::imager_model::TargetDrive,
    pub(super) raw: &'a File,
    pub(super) scratch: &'a Path,
    pub(super) daemon: &'a Udisks,
    pub(super) source_partition: &'a str,
    pub(super) output_partition: &'a str,
}
pub(super) fn complete_and_restore(
    setup: &Arc<PreparedSetupReview>,
    transaction: Arc<OfflineOutputTransaction>,
    read: Arc<OfflineReadAdmission>,
    medium: Arc<NativeMediumOutput>,
    subjects: Subjects<'_>,
) {
    let cancel = AtomicBool::new(false);
    // The admission above has genuinely reopened protected source and inventory
    // and constructed the delegated SourceScopedRunner through both validators.
    // It must be applications-only, not a fabricated Home export permission.
    assert!(read.output_declaration(&cancel).is_err());
    let admitted = read.output_applications_declaration(&cancel).unwrap();
    assert!(!admitted.source().categories.home);
    assert!(admitted.source().categories.flatpak);
    assert!(
        crate::setup::migration_handoff::OriginalInventoryBinding::parse(admitted.source())
            .unwrap()
            .is_some()
    );
    let original_read = read.output_original_home(&cancel).unwrap();
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    use std::os::fd::AsRawFd as _;
    // SAFETY: genuine retained original source descriptor and writable output.
    assert_eq!(
        unsafe { libc::fstatvfs(original_read.as_raw_fd(), status.as_mut_ptr()) },
        0
    );
    // SAFETY: successful fstatvfs initialized the entire value.
    assert_ne!(unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY, 0);
    let before_state = source_state(&original_read);
    drop(original_read);
    let outcome = transaction.export_applications_and_verify(Zeroizing::new(PASSWORD.to_vec()));
    let token = transaction
        .validate_verified_applications_outcome(&outcome)
        .expect("two genuine signed runs must mint applications authority");
    let expected = token.expected().clone();
    assert_eq!(
        expected.operation,
        token.declaration().applications_operation().unwrap()
    );
    let mut completed = setup
        .reseal_completed_applications(&token, Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    let manifest = completed.manifest();
    assert!(manifest.completed_applications_only);
    assert!(manifest.offline_applications.is_none() && manifest.offline_home.is_none());
    assert!(manifest.portable_home.is_none() && manifest.completed_home.is_none());
    assert_eq!(manifest.portable_flatpak.as_ref(), Some(&expected));
    assert_eq!(
        manifest.identity_public_key,
        setup.manifest().identity_public_key
    );
    let receipt = completed.publish_applications(&token, &cancel).unwrap();
    let retry = completed.publish_applications(&token, &cancel).unwrap();
    assert_eq!(
        receipt.setup, retry.setup,
        "actual readback must recover same immutable package"
    );
    let original = read.output_original_home(&cancel).unwrap();
    assert_eq!(
        source_state(&original),
        before_state,
        "signed export must leave selected and excluded original state unchanged"
    );
    assert_eq!(
        read_member(
            &original,
            ".var/app/org.tilo.Fixture/data/selected-document"
        ),
        b"genuine selected application state"
    );
    assert_eq!(
        read_member(&original, "owner-document"),
        b"unchanged original owner bytes"
    );
    drop(original);
    drop(completed);
    drop(token);
    drop(transaction);
    drop(read);
    let (shutdown, key, work) = outcome.cleanup_receipt().unwrap();
    medium.finish(shutdown, key, work).unwrap();
    assert!(
        subjects
            .daemon
            .filesystem(subjects.output_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    assert!(
        subjects
            .daemon
            .filesystem(subjects.source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    let destination = subjects.scratch.join("applications-only-consumer");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!Path::new("/run/tilo-installer").exists());
    fs::create_dir("/run/tilo-installer").unwrap();
    let mounted = Command::new("/usr/bin/mount")
        .args([
            "-t",
            "tmpfs",
            "-o",
            "mode=700,nosuid,nodev",
            "tmpfs",
            "/run/tilo-installer",
        ])
        .output()
        .unwrap();
    assert!(
        mounted.status.success(),
        "{}",
        String::from_utf8_lossy(&mounted.stderr)
    );
    crate::setup::media_setup::boot_medium::with_boot_medium(
        subjects.target,
        subjects.raw,
        &cancel,
        |medium| {
            let review = CompletedBootReview::review_if_committed(medium, PASSWORD, &cancel)?
                .expect("actual committed apps selection must recover");
            let capsule = review.retained_setup(&cancel)?;
            assert!(
                capsule
                    .authenticated_setup()
                    .manifest()
                    .completed_applications_only
            );
            assert!(
                capsule
                    .authenticated_setup()
                    .manifest()
                    .completed_home
                    .is_none()
            );
            fs::copy(capsule.package_path(), destination.join("setup.sealed"))?;
            review.adopt_install_generation(&cancel)
        },
    )
    .unwrap();
    assert!(
        completion::selected_generation()
            .unwrap()
            .starts_with("/run/tilo-installer/.completed-setup-")
    );
    assert!(
        completion::stream_payload("home", &mut std::io::sink(), &cancel).is_err(),
        "apps selection grants no Home stream"
    );
    let mut ciphertext = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination.join(&expected.filename))
        .unwrap();
    completion::stream_payload("applications", &mut ciphertext, &cancel).unwrap();
    ciphertext.sync_all().unwrap();
    drop(ciphertext);
    assert_eq!(
        <[u8; 32]>::from(sha2::Sha256::digest(
            fs::read(destination.join(&expected.filename)).unwrap()
        )),
        expected.ciphertext_sha256
    );
    fs::write(
        destination.join("expected.json"),
        serde_json::to_vec(&expected).unwrap(),
    )
    .unwrap();
    let source_hashes: Vec<(String, [u8; 32])> = before_state
        .iter()
        .map(|(name, bytes)| (name.clone(), sha2::Sha256::digest(bytes).into()))
        .collect();
    fs::write(
        destination.join("source-state-sha256.json"),
        serde_json::to_vec(&source_hashes).unwrap(),
    )
    .unwrap();
    for entry in fs::read_dir(&destination).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    own_source(&destination);
    fs::set_permissions(subjects.scratch, fs::Permissions::from_mode(0o711)).unwrap();
    let mut child = super::paired::owner_child("restore-applications", &destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(PASSWORD).unwrap();
    super::paired::require_child(child.wait_with_output().unwrap());
    assert!(
        subjects
            .daemon
            .filesystem(subjects.output_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
    assert!(
        subjects
            .daemon
            .filesystem(subjects.source_partition)
            .and_then(|proxy| proxy.get_property::<Vec<Vec<u8>>>("MountPoints"))
            .unwrap()
            .is_empty()
    );
}
pub(super) fn restore_child() {
    let root = PathBuf::from(std::env::var_os("TILO_ORIGINAL_PAIRED_ROOT").unwrap());
    let mut password = Zeroizing::new(Vec::new());
    std::io::stdin()
        .take(4097)
        .read_to_end(&mut password)
        .unwrap();
    assert_eq!(password.as_slice(), PASSWORD);
    let cancel = AtomicBool::new(false);
    let setup = PreparedSetupReview::open(
        &root.join("setup.sealed"),
        &password,
        &root.join("authenticated"),
        &cancel,
    )
    .unwrap();
    setup.require_completed_export().unwrap();
    assert!(setup.manifest().completed_applications_only);
    assert!(setup.manifest().portable_home.is_none() && setup.manifest().completed_home.is_none());
    let expected: crate::setup::flatpak_transfer::portable::Expectation =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    assert_eq!(setup.manifest().portable_flatpak.as_ref(), Some(&expected));
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).unwrap();
    let config = home.join(".config/tilo");
    fs::create_dir(home.join(".config")).unwrap();
    fs::set_permissions(home.join(".config"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    let identity_path = setup.import_identity(&config, &password).unwrap();
    let signer = runtime_identity::load_from_path(identity_path).unwrap();
    assert_eq!(
        signer.group_public_key(),
        setup.manifest().identity_public_key
    );
    let workspace = home
        .join(".local/state/tilo/portable-applications")
        .join(&expected.operation);
    fs::create_dir_all(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    let mut apps = ApplicationImport::review(
        &setup,
        &root.join(&expected.filename),
        &password,
        &workspace.join("imported"),
        &cancel,
    )
    .unwrap();
    assert_eq!(apps.manifest().schema, 2);
    assert_eq!(apps.manifest().deployments.len(), 2);
    assert_eq!(apps.manifest().app_state.len(), 1);
    let selected_state = &apps.manifest().app_state[0];
    assert_eq!(selected_state.app_id, "org.tilo.Fixture");
    assert!(selected_state.data_present && selected_state.override_present);
    if super::paired::user_applications() {
        assert!(apps.manifest().selected.iter().all(|item| item.installation == crate::setup::flatpak_transfer::Installation::User));
        assert!(
            apps.manifest()
                .deployments
                .iter()
                .all(|item| item.installed.installation
                    == crate::setup::flatpak_transfer::Installation::User)
        );
    }
    apps.adopt_user_state(&NativeOfflineRunner, &home, &workspace, &cancel)
        .unwrap();
    apps.restore(&NativeOfflineRunner, &home, &cancel).unwrap();
    verify_selected_state(&home, &root);
    assert_eq!(
        fs::read(home.join(".var/app/org.tilo.Fixture/data/selected-document")).unwrap(),
        b"genuine selected application state"
    );
    for name in [
        "owner-document",
        "group-only",
        "same-document",
        ".var/app/so.test/data/document",
    ] {
        assert!(
            !home.join(name).exists(),
            "unselected Home path must not be adopted: {name}"
        );
    }
    for deployment in &apps.manifest().deployments {
        let actual = NativeOfflineRunner
            .run(
                &home,
                &[
                    "info".into(),
                    "--user".into(),
                    "--show-commit".into(),
                    deployment.installed.reference.clone(),
                ],
            )
            .unwrap();
        assert_eq!(actual.trim(), deployment.commit);
    }
    apps.adopt_user_state(&NativeOfflineRunner, &home, &workspace, &cancel)
        .unwrap();
    apps.restore(&NativeOfflineRunner, &home, &cancel).unwrap();
    verify_selected_state(&home, &root);
    assert_eq!(
        fs::read(home.join(".var/app/org.tilo.Fixture/data/selected-document")).unwrap(),
        b"genuine selected application state"
    );
}
