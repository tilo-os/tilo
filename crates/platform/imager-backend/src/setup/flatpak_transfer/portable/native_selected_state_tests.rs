//! Real selected state through the canonical library exporter and native consumer.
//! This does not qualify the signed installer worker, media output or app launch.
use super::*;
use crate::setup::flatpak_transfer::InstalledRef;
const DATABASE: &str = ".var/app/org.tilo.Fixture/data/session.sqlite";
const DOCUMENT: &str = ".var/app/org.tilo.Fixture/data/document";
const OVERRIDE: &str = ".local/share/flatpak/overrides/org.tilo.Fixture";
const EXCLUDED_DATA: &str = ".var/app/org.tilo.Excluded/data/document";
const EXCLUDED_OVERRIDE: &str = ".local/share/flatpak/overrides/org.tilo.Excluded";
const MEMBERS: [&str; 5] = [
    DATABASE,
    DOCUMENT,
    OVERRIDE,
    EXCLUDED_DATA,
    EXCLUDED_OVERRIDE,
];
fn source_fixture(home: &Path) {
    private(&home.join(".var/app/org.tilo.Fixture/data"));
    let database = rusqlite::Connection::open(home.join(DATABASE)).unwrap();
    database.execute_batch(
        "CREATE TABLE session (workspace TEXT NOT NULL, revision INTEGER NOT NULL);\n         INSERT INTO session VALUES ('original selected workspace', 17);",
    ).unwrap();
    database.close().unwrap();
    fs::write(
        home.join(DOCUMENT),
        b"original selected application document",
    )
    .unwrap();
    private(&home.join(".var/app/org.tilo.Excluded/data"));
    fs::write(
        home.join(EXCLUDED_DATA),
        b"excluded original application data",
    )
    .unwrap();
    private(&home.join(".local/share/flatpak/overrides"));
    fs::write(
        home.join(OVERRIDE),
        b"[Environment]\nTILO_SELECTED_FIXTURE=original-setting\n",
    )
    .unwrap();
    fs::write(
        home.join(EXCLUDED_OVERRIDE),
        b"[Environment]\nTILO_EXCLUDED_FIXTURE=excluded-setting\n",
    )
    .unwrap();
}
fn witness(home: &Path) -> Vec<Vec<u8>> {
    MEMBERS
        .iter()
        .map(|name| fs::read(home.join(name)).unwrap())
        .collect()
}
fn verify_target(home: &Path, before: &[Vec<u8>]) {
    for (name, bytes) in MEMBERS.iter().zip(before) {
        if matches!(*name, EXCLUDED_DATA | EXCLUDED_OVERRIDE) {
            assert!(
                !home.join(name).exists(),
                "unselected application state adopted"
            );
        } else {
            assert_eq!(
                fs::read(home.join(name)).unwrap(),
                *bytes,
                "selected original bytes changed"
            );
        }
    }
    assert!(!home.join(".var/app/org.tilo.Excluded").exists());
    let database = rusqlite::Connection::open_with_flags(
        home.join(DATABASE),
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
    let settings = native(home, &["override", "--user", "--show", "org.tilo.Fixture"]);
    assert_eq!(
        settings
            .lines()
            .filter(|line| *line == "TILO_SELECTED_FIXTURE=original-setting")
            .count(),
        1
    );
}
pub(super) fn owner(source_home: &Path, app: InstalledRef) {
    assert_ne!(
        rustix::process::geteuid().as_raw(),
        0,
        "ordinary source and target credentials required"
    );
    source_fixture(source_home);
    let before = witness(source_home);
    let source =
        PortableFlatpakSource::review_selected_state(&NativeRunner, source_home, vec![app])
            .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    private(&root);
    let stage = root.join("stage");
    private(&stage);
    private(&root.join("journals"));
    let cancel = AtomicBool::new(false);
    let staged = source
        .stage(
            &NativeOfflineRunner,
            StageGrant {
                root: &stage,
                journals: &root.join("journals"),
                allocation_bytes: 256 << 20,
                include_user_state: true,
            },
            &cancel,
        )
        .unwrap();
    assert!(!stage.join("app-state/org.tilo.Excluded").exists());
    let operation = uuid::Uuid::new_v4().to_string();
    let package = root.join(format!("applications-{operation}.sealed"));
    let expected = staged
        .seal(
            &package,
            &operation,
            "native-system-binding",
            Zeroizing::new(PASSWORD.to_vec()),
            Limits {
                plaintext_bytes: 128 << 20,
                entries: 8192,
                metadata_bytes: 8 << 20,
            },
            &cancel,
        )
        .unwrap();
    assert_eq!(expected.consumer_protocol, 2);
    let setup = prepared_setup(&root, expected, &cancel);
    let target = root.join("target");
    private(&target);
    for child in [
        ".local",
        ".local/state",
        ".local/state/tilo",
        ".local/state/tilo/portable-applications",
    ] {
        private(&target.join(child));
    }
    let workspace = target
        .join(".local/state/tilo/portable-applications")
        .join(&operation);
    private(&workspace);
    let mut import = ApplicationImport::review(
        &setup,
        &package,
        PASSWORD,
        &workspace.join("imported"),
        &cancel,
    )
    .unwrap();
    assert_eq!(import.manifest().schema, 2);
    assert!(!import.manifest().user_state);
    assert_eq!(import.manifest().deployments.len(), 2);
    assert_eq!(import.manifest().app_state.len(), 1);
    let state = &import.manifest().app_state[0];
    assert_eq!(state.app_id, "org.tilo.Fixture");
    assert!(state.data_present && state.override_present);
    assert!(
        import
            .manifest()
            .deployments
            .iter()
            .all(|item| item.installed.installation == Installation::System(SCOPE.into()))
    );
    import
        .adopt_user_state(&NativeOfflineRunner, &target, &workspace, &cancel)
        .unwrap();
    assert_eq!(
        import
            .restore(&NativeOfflineRunner, &target, &cancel)
            .unwrap(),
        2
    );
    for reference in [RUNTIME, APP] {
        let original = NativeRunner
            .run(
                source_home,
                &[
                    "info".into(),
                    Installation::System(SCOPE.into()).argument(),
                    "--show-commit".into(),
                    reference.into(),
                ],
            )
            .unwrap();
        assert_eq!(
            native(&target, &["info", "--user", "--show-commit", reference]).trim(),
            original.trim()
        );
    }
    NativeOfflineRunner.verify_repository(&target).unwrap();
    verify_target(&target, &before);
    assert_eq!(
        witness(source_home),
        before,
        "capture and restore changed original source bytes"
    );
    source.validate().unwrap();
    import
        .adopt_user_state(&NativeOfflineRunner, &target, &workspace, &cancel)
        .unwrap();
    assert_eq!(
        import
            .restore(&NativeOfflineRunner, &target, &cancel)
            .unwrap(),
        2
    );
    verify_target(&target, &before);
    assert_eq!(
        witness(source_home),
        before,
        "idempotent retry changed original source bytes"
    );
    source.validate().unwrap();
}
