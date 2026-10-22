use super::*;
use crate::setup::{
    flatpak_transfer::{
        Installation, InstalledRef, Remote, Runner,
        portable::{ApplicationsManifest, Deployment, Trust},
    },
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest, PortableHomePlan, PortableHomeSelection},
};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt};
const PASSWORD: &[u8] = b"several unrelated installation password words";
const APP: &str = "app/org.example.Paint/x86_64/stable";
const REMOTE: &str = "tilo-portable-0";
struct Inert {
    calls: RefCell<Vec<Vec<String>>>,
    installed: RefCell<bool>,
    refuse_signature: bool,
}
impl Runner for Inert {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        match args[0].as_str() {
            "list"
                if home.join(".local/share/flatpak/repo/restored").exists()
                    && args.contains(&"--app".into()) =>
            {
                let origin =
                    fs::read_to_string(home.join(".local/share/flatpak/repo/origin")).unwrap();
                if args.iter().any(|arg| arg.contains("installation:f")) {
                    Ok(format!(
                        "org.example.Paint/x86_64/stable\t{origin}\tuser\tPaint\n"
                    ))
                } else {
                    Ok(format!("org.example.Paint/x86_64/stable\t{origin}\n"))
                }
            }
            "list" | "remotes" => Ok(String::new()),
            "remote-add" => {
                fs::create_dir_all(home.join(".local/share/flatpak/repo")).unwrap();
                Ok(String::new())
            }
            "install" => {
                assert!(args.contains(&"--no-pull".into()));
                assert!(args.contains(&"--no-deps".into()));
                assert!(args.contains(&"--no-related".into()));
                *self.installed.borrow_mut() = true;
                fs::write(
                    home.join(".local/share/flatpak/repo/restored"),
                    b"inert deployment",
                )
                .unwrap();
                fs::write(
                    home.join(".local/share/flatpak/repo/origin"),
                    &args[args.len() - 2],
                )
                .unwrap();
                Ok(String::new())
            }
            "info" if args.contains(&"--show-origin".into()) => {
                fs::read_to_string(home.join(".local/share/flatpak/repo/origin"))
                    .map_err(|error| error.to_string())
            }
            "info" => Ok("a".repeat(64)),
            _ => Err("Unexpected fixture command".into()),
        }
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        if home.join(".local/share/flatpak/repo").is_dir() {
            Ok(())
        } else {
            Err("Missing fixture repository".into())
        }
    }
}
impl OfflineRunner for Inert {
    fn ostree(&self, _repository: &Path, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        if args[0] == "pull-local" {
            assert!(args.contains(&"--gpg-verify".into()));
            assert!(args.contains(&"--untrusted".into()));
            if self.refuse_signature {
                return Err("Signature refused".into());
            }
        }
        Ok(String::new())
    }
}
fn directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn file(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
struct Fixture {
    _environment: tilo_test_env::ScopedEnv,
    _temporary: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    package: PathBuf,
    setup: PathBuf,
    workspace: PathBuf,
    receipt: PathBuf,
    source: PathBuf,
    runner: Inert,
}
impl Fixture {
    fn new(home_selected: bool, user_state: bool) -> Self {
        Self::new_schema(home_selected, user_state, 1)
    }
    fn new_schema(home_selected: bool, user_state: bool, schema: u32) -> Self {
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        directory(&root);
        let home = root.join("target-home");
        directory(&home);
        directory(&home.join(".local/state/tilo"));
        let delivered = home.join(".local/state/tilo/prepared-applications");
        directory(&delivered);
        let source = root.join("source");
        directory(&source.join("installation-0/.ostree/repo"));
        file(
            &source.join("installation-0/.ostree/repo/object"),
            b"original signed object",
        );
        file(
            &source.join("installation-0/.ostree/repo/original.trustedkeys.gpg"),
            b"fixture signing key",
        );
        let source_remote = Remote {
            installation: Installation::System("default".into()),
            name: "original".into(),
            url: "https://repo.example.test/repo/".into(),
            options: String::new(),
        };
        let trust = crate::setup::flatpak_transfer::extract_remote_definition(
            &source_remote,
            &source.join("installation-0/.ostree"),
        )
        .unwrap();
        file(&source.join("approved.flatpakrepo"), &trust);
        if user_state {
            directory(&source.join("user-state"));
            directory(&source.join("user-state/.local/share/flatpak/repo"));
            directory(&source.join("user-state/.var/app/org.example.Paint"));
            file(
                &source.join("user-state/.var/app/org.example.Paint/session"),
                b"private session",
            );
        }
        let installed = InstalledRef {
            reference: APP.into(),
            origin: "original".into(),
            installation: Installation::System("default".into()),
            name: "Paint".into(),
        };
        if schema == 2 {
            assert!(!user_state && !home_selected);
            directory(&source.join("app-state/org.example.Paint/data"));
            file(
                &source.join("app-state/org.example.Paint/data/session"),
                b"selected private app state",
            );
        }
        let manifest = ApplicationsManifest {
            schema,
            selected: vec![installed.clone()],
            deployments: vec![Deployment {
                installed,
                commit: "a".repeat(64),
                repository: "installation-0".into(),
                remote: REMOTE.into(),
            }],
            trust: vec![Trust {
                remote: REMOTE.into(),
                definition: "approved.flatpakrepo".into(),
                sha256: Sha256::digest(&trust).into(),
                source: source_remote,
            }],
            user_state,
            app_state: if schema == 2 {
                vec![crate::setup::flatpak_transfer::portable::AppState {
                    app_id: "org.example.Paint".into(),
                    data_present: true,
                    override_present: false,
                }]
            } else {
                Vec::new()
            },
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        file(&source.join("applications.json"), &manifest_bytes);
        let operation = uuid::Uuid::new_v4().to_string();
        let package = delivered.join(format!("applications-{operation}.sealed"));
        let limits = Limits {
            plaintext_bytes: 16 * 1024 * 1024,
            entries: 2048,
            metadata_bytes: 2 * 1024 * 1024,
        };
        let cancel = AtomicBool::new(false);
        let info = ExportReview::review(&source, &package, Purpose::FlatpakApplications, limits)
            .unwrap()
            .export_bound(
                Zeroizing::new(PASSWORD.to_vec()),
                "prepared-binding",
                &cancel,
            )
            .unwrap();
        let digest = ImportReview::review(
            &package,
            PASSWORD,
            Purpose::FlatpakApplications,
            limits,
            &cancel,
        )
        .unwrap()
        .ciphertext_digest();
        let expected = Expectation {
            schema: 1,
            consumer_protocol: manifest.schema,
            operation: operation.clone(),
            purpose: Purpose::FlatpakApplications,
            filename: package.file_name().unwrap().to_str().unwrap().into(),
            setup_binding: "prepared-binding".into(),
            sealed_bytes: info.sealed_bytes,
            plaintext_bytes: info.plaintext_bytes,
            entries: info.entries,
            ciphertext_sha256: digest,
            manifest_sha256: Sha256::digest(&manifest_bytes).into(),
            limits,
        };
        let receipt = delivered.join("expectation.json");
        file(&receipt, &serde_json::to_vec(&expected).unwrap());
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [1, 2, 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let stage = root.join("setup-stage");
        directory(&stage);
        identity
            .export(
                &stage.join(prepared_setup::IDENTITY_FILE),
                Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            )
            .unwrap();
        let mut prepared =
            Manifest::new("prepared-binding".into(), identity.public_key(), vec![]).unwrap();
        prepared.portable_flatpak = Some(expected);
        if home_selected {
            prepared.portable_home = Some(PortableHomePlan {
                schema: 1,
                selection: PortableHomeSelection::FullHome,
                applications: false,
            });
        }
        prepared.write(&stage).unwrap();
        let setup = root.join("setup.sealed");
        ExportReview::review(
            &stage,
            &setup,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
        directory(&home.join(".config/tilo"));
        let activated =
            PreparedSetupReview::open(&setup, PASSWORD, &root.join("initial-activation"), &cancel)
                .unwrap();
        activated
            .import_identity(&home.join(".config/tilo"), PASSWORD)
            .unwrap();
        let workspace = home
            .join(".local/state/tilo/portable-applications")
            .join(operation);
        directory(&workspace);
        Self {
            _environment: environment,
            _temporary: temporary,
            root,
            home,
            package,
            setup,
            workspace,
            receipt,
            source,
            runner: Inert {
                calls: RefCell::default(),
                installed: RefCell::new(false),
                refuse_signature: false,
            },
        }
    }
    fn args(&self, action: &str, attempt: &str) -> Vec<OsString> {
        let quarantine = self.root.join(attempt);
        [
            ("--setup-package", self.setup.as_os_str()),
            ("--setup-quarantine", quarantine.as_os_str()),
            ("--package", self.package.as_os_str()),
            ("--receipt", self.receipt.as_os_str()),
            ("--home", self.home.as_os_str()),
            ("--workspace", self.workspace.as_os_str()),
            ("--action", std::ffi::OsStr::new(action)),
        ]
        .into_iter()
        .flat_map(|(key, value)| [OsString::from(key), value.to_owned()])
        .collect()
    }
    fn run(&self, action: &str, attempt: &str) -> io::Result<Vec<u8>> {
        let mut output = Vec::new();
        run_with_runner(
            self.args(action, attempt),
            &mut &PASSWORD[..],
            &mut output,
            &self.runner,
        )?;
        Ok(output)
    }
    fn adopt_home(&self) {
        use crate::setup::portable_home_media::{HomeSource, MediaGrant};
        let source = self.root.join("original-home");
        directory(&source);
        directory(&source.join(".local/share/flatpak/repo"));
        directory(&source.join(".var/app/org.example.Paint"));
        file(&source.join("document"), b"original document");
        file(
            &source.join(".var/app/org.example.Paint/session"),
            b"original session",
        );
        file(
            &source.join(".local/share/flatpak/repo/config"),
            b"[core]\nmode=bare-user-only\n",
        );
        file(
            &source.join(".local/share/flatpak/repo/original-object"),
            b"original user object",
        );
        let delivered = self.home.join(".local/state/tilo/prepared-home");
        directory(&delivered);
        let cancel = AtomicBool::new(false);
        let expected = HomeSource::review(&source)
            .unwrap()
            .export_to(
                &MediaGrant::open(&delivered).unwrap(),
                "home-operation",
                "prepared-binding",
                Zeroizing::new(PASSWORD.to_vec()),
                &cancel,
            )
            .unwrap();
        file(
            &delivered.join("expectation.json"),
            &serde_json::to_vec(&expected).unwrap(),
        );
        let workspace = self
            .home
            .join(".local/state/tilo/portable-home")
            .join(&expected.operation);
        directory(&workspace);
        let setup = PreparedSetupReview::open(
            &self.setup,
            PASSWORD,
            &self.root.join("home-setup-review"),
            &cancel,
        )
        .unwrap();
        let mut review = PortableHomeReview::review(
            HomeImportGrants {
                package: delivered.join(&expected.filename),
                home: self.home.clone(),
                workspace,
            },
            &setup,
            &expected,
            PASSWORD,
            &cancel,
        )
        .unwrap();
        review
            .prepare(Zeroizing::new(PASSWORD.to_vec()), &cancel)
            .unwrap();
        let mut worker = review.start_adoption(&cancel).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        loop {
            if let Some(result) = worker.try_next() {
                assert!(result.unwrap().complete);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        review.verify_completed_adoption(&cancel).unwrap();
    }
}
#[test]
fn review_restore_and_retry_revalidate_actual_archive_and_offline_target() {
    let fixture = Fixture::new(false, false);
    let original = fs::read(fixture.source.join("installation-0/.ostree/repo/object")).unwrap();
    let review = fixture.run("review", "review").unwrap();
    assert!(
        String::from_utf8(review)
            .unwrap()
            .contains("ready-to-restore")
    );
    assert!(fixture.runner.calls.borrow().is_empty());
    assert!(!fixture.workspace.join("restore-complete.json").exists());
    let output = fixture.run("restore", "restore").unwrap();
    let final_event: serde_json::Value = serde_json::from_slice(
        output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .last()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        final_event,
        serde_json::json!({"event":"complete","restored":1,"verified":true})
    );
    fixture.run("restore", "retry").unwrap();
    assert_eq!(
        fixture
            .runner
            .calls
            .borrow()
            .iter()
            .filter(|args| args[0] == "install")
            .count(),
        1
    );
    assert_eq!(
        fs::read(fixture.source.join("installation-0/.ostree/repo/object")).unwrap(),
        original
    );
    file(
        &fixture
            .workspace
            .join("imported/home/installation-0/.ostree/repo/object"),
        b"tampered",
    );
    fixture.runner.calls.borrow_mut().clear();
    assert!(fixture.run("restore", "tampered-retry").is_err());
    assert!(fixture.runner.calls.borrow().is_empty());
}
#[test]
fn wrong_password_changed_declaration_and_signature_refusal_never_complete() {
    let mut fixture = Fixture::new(false, false);
    let mut output = Vec::new();
    assert!(
        run_with_runner(
            fixture.args("restore", "wrong"),
            &mut &b"wrong"[..],
            &mut output,
            &fixture.runner
        )
        .is_err()
    );
    assert!(output.is_empty());
    let original = fs::read(&fixture.receipt).unwrap();
    let mut expected: Expectation = serde_json::from_slice(&original).unwrap();
    expected.ciphertext_sha256[0] ^= 1;
    file(&fixture.receipt, &serde_json::to_vec(&expected).unwrap());
    assert!(fixture.run("restore", "changed").is_err());
    assert!(fixture.runner.calls.borrow().is_empty());
    file(&fixture.receipt, &original);
    fixture.runner.refuse_signature = true;
    assert!(fixture.run("restore", "signature").is_err());
    assert!(fixture.workspace.join("restore-started.json").exists());
    assert!(!fixture.workspace.join("restore-complete.json").exists());
    assert!(!*fixture.runner.installed.borrow());
}
#[test]
fn selected_home_cannot_be_bypassed_by_app_restore_or_completion_marker() {
    let fixture = Fixture::new(true, false);
    file(
        &fixture.workspace.join("restore-complete.json"),
        b"{\"complete\":true}",
    );
    assert!(fixture.run("restore", "without-home").is_err());
    assert!(fixture.runner.calls.borrow().is_empty());
    assert!(!fixture.workspace.join("imported").exists());
}
#[test]
fn apps_only_adopts_private_user_state_without_importing_documents() {
    let fixture = Fixture::new(false, true);
    file(&fixture.home.join("document"), b"target document");
    fixture.run("restore", "apps-only").unwrap();
    assert_eq!(
        fs::read(fixture.home.join(".var/app/org.example.Paint/session")).unwrap(),
        b"private session"
    );
    assert_eq!(
        fs::read(fixture.home.join("document")).unwrap(),
        b"target document"
    );
    assert_eq!(
        fs::read(
            fixture
                .source
                .join("user-state/.var/app/org.example.Paint/session")
        )
        .unwrap(),
        b"private session"
    );
    fixture.run("restore", "apps-only-retry").unwrap();
}
#[test]
fn full_home_restore_replays_after_actual_repository_changes() {
    let fixture = Fixture::new(true, false);
    fixture.adopt_home();
    fixture.run("review", "full-home-review").unwrap();
    fixture.run("restore", "full-home-restore").unwrap();
    assert!(
        fixture
            .home
            .join(".local/share/flatpak/repo/restored")
            .exists()
    );
    fixture.run("review", "full-home-review-retry").unwrap();
    fixture.run("restore", "full-home-restore-retry").unwrap();
    assert_eq!(
        fixture
            .runner
            .calls
            .borrow()
            .iter()
            .filter(|args| args[0] == "install")
            .count(),
        1
    );
    assert_eq!(
        fs::read(fixture.home.join("document")).unwrap(),
        b"original document"
    );
    assert_eq!(
        fs::read(fixture.home.join(".var/app/org.example.Paint/session")).unwrap(),
        b"original session"
    );
    assert_eq!(
        fs::read(fixture.root.join("original-home/document")).unwrap(),
        b"original document"
    );
}
/// A retry after a full-home restoration refuses to run once `path` changed
/// underneath it, leaves the change alone and installs nothing. One test per
/// path: each builds and restores its own sealed fixture, and the three in
/// one test summed past the per-test time limit on a loaded node.
fn full_home_retry_refuses_after(path: &str) {
    let fixture = Fixture::new(true, false);
    fixture.adopt_home();
    fixture.run("restore", "full-home-restore").unwrap();
    file(&fixture.home.join(path), b"changed after restoration");
    fixture.runner.calls.borrow_mut().clear();
    assert!(fixture.run("restore", "changed").is_err());
    assert_eq!(
        fs::read(fixture.home.join(path)).unwrap(),
        b"changed after restoration"
    );
    assert!(
        !fixture
            .runner
            .calls
            .borrow()
            .iter()
            .any(|args| args[0] == "install")
    );
}
#[test]
fn full_home_retry_refuses_changed_original_data() {
    full_home_retry_refuses_after("document");
}
#[test]
fn full_home_retry_refuses_changed_application_data() {
    full_home_retry_refuses_after(".var/app/org.example.Paint/session");
}
#[test]
fn full_home_retry_refuses_changed_signed_output() {
    full_home_retry_refuses_after(".local/share/flatpak/repo/restored");
}

#[test]
fn authenticated_selected_app_settings_refuse_unqualified_consumer_before_restore_publication() {
    let fixture = Fixture::new_schema(false, false, 2);
    let mut output = Vec::new();
    let called = std::cell::Cell::new(false);
    assert!(
        run_with_admission(
            fixture.args("restore", "missing-selected-consumer"),
            &mut &PASSWORD[..],
            &mut output,
            &fixture.runner,
            || {
                called.set(true);
                Err(io::ErrorKind::Unsupported.into())
            }
        )
        .is_err()
    );
    assert!(
        called.get(),
        "Authenticated schema2 must reach distinct target admission"
    );
    assert!(output.is_empty());
    assert!(fixture.runner.calls.borrow().is_empty());
    assert!(!fixture.workspace.join("restore-started.json").exists());
    assert!(!fixture.workspace.join("restore-complete.json").exists());
    assert!(!fixture.workspace.join("imported").exists());
    assert!(!fixture.home.join(".var/app").exists());
    assert!(!fixture.home.join(".local/share/flatpak/repo").exists());
}
