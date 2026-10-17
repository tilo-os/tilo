use super::*;
#[cfg(test)]
use crate::setup::{
    portable_transfer::{ExportReview, Limits, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest, PreparedSetupReview},
};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{cell::RefCell, fs, os::unix::fs::PermissionsExt};
#[cfg(test)]
use zeroize::Zeroizing;
#[cfg(test)]
const PASSWORD: &[u8] = b"a long prepared device password";
const APP: &str = "app/org.example.Paint/x86_64/stable";
const RUNTIME: &str = "runtime/org.example.Platform/x86_64/stable";
pub struct Fake {
    pub source_home: PathBuf,
    original: PathBuf,
    calls: RefCell<Vec<Vec<String>>>,
    installed: RefCell<BTreeSet<String>>,
    fail_signature: bool,
}
impl Runner for Fake {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        let source = home == self.source_home;
        match args.first().map(String::as_str) {
            Some("list") if source => Ok(if args.contains(&"--app".into()) {
                "org.example.Paint/x86_64/stable\tflathub\tsystem\tPaint\n".into()
            } else {
                "org.example.Platform/x86_64/stable\tflathub\tsystem\tPlatform\n".into()
            }),
            Some("list") => {
                let kind = if args.contains(&"--app".into()) {
                    "app/"
                } else {
                    "runtime/"
                };
                Ok(self
                    .installed
                    .borrow()
                    .iter()
                    .filter_map(|item| item.strip_prefix(kind))
                    .fold(String::new(), |mut output, item| {
                        output.push_str(item);
                        output.push_str("\ttilo-portable-0\tuser\tRestored\n");
                        output
                    }))
            }
            Some("remotes") if source && args.contains(&"--installation=default".into()) => {
                Ok("flathub\thttps://repo.example.test/repo/\t\n".into())
            }
            Some("remotes") => Ok(String::new()),
            Some("info") if args.contains(&"--show-location".into()) => Ok(self
                .original
                .join(args.last().unwrap())
                .join("a".repeat(64))
                .to_str()
                .unwrap()
                .into()),
            Some("info") if args.contains(&"--show-size".into()) => Ok("128".into()),
            Some("info") if args.contains(&"--show-runtime".into()) => {
                Ok("org.example.Platform/x86_64/stable".into())
            }
            Some("info") if args.contains(&"--show-extensions".into()) => {
                Ok(format!("{}\n", args.last().unwrap()))
            }
            Some("info") if args.contains(&"--show-origin".into()) => Ok("tilo-portable-0".into()),
            Some("info") => Ok("a".repeat(64)),
            Some("create-usb") => {
                let directory = Path::new(args.get(2).unwrap()).join(".ostree/repo");
                fs::create_dir_all(&directory).unwrap();
                fs::write(directory.join("signed-object"), b"fixture signed objects").unwrap();
                Ok(String::new())
            }
            Some("remote-add") => {
                fs::create_dir_all(home.join(".local/share/flatpak/repo")).unwrap();
                Ok(String::new())
            }
            Some("install") => {
                assert!(args.contains(&"--no-pull".into()));
                assert!(args.contains(&"--no-deps".into()));
                assert!(args.contains(&"--no-related".into()));
                self.installed
                    .borrow_mut()
                    .insert(args.last().unwrap().clone());
                fs::write(
                    home.join(".local/share/flatpak/repo/restored-object"),
                    b"verified offline object",
                )
                .unwrap();
                Ok(String::new())
            }
            _ => Err("Unexpected fixture command".into()),
        }
    }
    fn verify_repository(&self, _home: &Path) -> Result<(), String> {
        Ok(())
    }
}
impl OfflineRunner for Fake {
    fn ostree(&self, _repo: &Path, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        if args.contains(&"--collections".into()) {
            return Ok(format!(
                "(org.example.Repo, {APP})\t{}\n(org.example.Repo, {RUNTIME})\t{}\n",
                "a".repeat(64),
                "a".repeat(64)
            ));
        }
        if args.first().is_some_and(|verb| verb == "pull-local") {
            assert!(args.contains(&"--untrusted".into()));
            assert!(args.contains(&"--gpg-verify".into()));
            assert!(args.contains(&"--remote=tilo-portable-0".into()));
            if self.fail_signature {
                return Err("Fixture signature refusal".into());
            }
        }
        Ok(String::new())
    }
}
fn private(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
pub fn fixture(root: &Path) -> Fake {
    private(root);
    let home = root.join("source-home");
    private(&home);
    let original = root.join("original-installation");
    private(&original);
    private(&original.join("repo"));
    fs::write(
        original.join("repo/flathub.trustedkeys.gpg"),
        b"fixture-public-signing-key",
    )
    .unwrap();
    Fake {
        source_home: home,
        original,
        calls: RefCell::default(),
        installed: RefCell::default(),
        fail_signature: false,
    }
}
#[cfg(test)]
fn limits() -> Limits {
    Limits {
        plaintext_bytes: 64 * 1024 * 1024,
        entries: 4096,
        metadata_bytes: 8 * 1024 * 1024,
    }
}
#[cfg(test)]
struct OmittedRuntime<'a>(&'a Fake);
#[cfg(test)]
impl Runner for OmittedRuntime<'_> {
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        self.0.run(home, arguments)
    }
}
#[cfg(test)]
impl OfflineRunner for OmittedRuntime<'_> {
    fn ostree(&self, repo: &Path, arguments: &[String]) -> Result<String, String> {
        let result = self.0.ostree(repo, arguments)?;
        if arguments.contains(&"--collections".into()) {
            return Ok(result.lines().filter(|line| !line.contains(RUNTIME)).fold(
                String::new(),
                |mut output, line| {
                    output.push_str(line);
                    output.push('\n');
                    output
                },
            ));
        }
        Ok(result)
    }
}
#[test]
fn portable_stage_refuses_exporter_omitting_reviewed_runtime() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    let fake = fixture(&root);
    let selected = InstalledRef {
        reference: APP.into(),
        origin: "flathub".into(),
        installation: Installation::System("default".into()),
        name: "Paint".into(),
    };
    let source = PortableFlatpakSource::review(&fake, &fake.source_home, vec![selected]).unwrap();
    let stage = root.join("incomplete-stage");
    let journals = root.join("incomplete-journals");
    private(&stage);
    private(&journals);
    let error = match source.stage(
        &OmittedRuntime(&fake),
        StageGrant {
            root: &stage,
            journals: &journals,
            allocation_bytes: 64 * 1024 * 1024,
            include_user_state: false,
        },
        &AtomicBool::new(false),
    ) {
        Ok(_) => panic!("An incomplete runtime export was marked ready"),
        Err(error) => error,
    };
    assert!(error.to_string().contains(RUNTIME));
    assert!(!stage.join("applications.json").exists());
}
#[cfg(test)]
fn package(root: &Path, fake: &Fake, state: bool) -> (PathBuf, Expectation) {
    let selected = InstalledRef {
        reference: APP.into(),
        origin: "flathub".into(),
        installation: Installation::System("default".into()),
        name: "Paint".into(),
    };
    let source = PortableFlatpakSource::review(fake, &fake.source_home, vec![selected]).unwrap();
    let stage = root.join("apps-stage");
    private(&stage);
    let journals = root.join("source-journals");
    private(&journals);
    let staged = source
        .stage(
            fake,
            StageGrant {
                root: &stage,
                journals: &journals,
                allocation_bytes: 64 * 1024 * 1024,
                include_user_state: state,
            },
            &AtomicBool::new(false),
        )
        .unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let path = root.join(format!("applications-{operation}.sealed"));
    let expected = staged
        .seal(
            &path,
            &operation,
            "prepared-binding",
            Zeroizing::new(PASSWORD.to_vec()),
            limits(),
            &AtomicBool::new(false),
        )
        .unwrap();
    (path, expected)
}
#[cfg(test)]
fn setup(root: &Path, expected: Expectation) -> PreparedSetupReview {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [1, 2, 3],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let directory = root.join("prepared");
    private(&directory);
    identity
        .export(
            &directory.join(prepared_setup::IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let mut manifest =
        Manifest::new("prepared-binding".into(), identity.public_key(), vec![]).unwrap();
    manifest.portable_flatpak = Some(expected);
    manifest.write(&directory).unwrap();
    let path = root.join("setup.sealed");
    ExportReview::review(
        &directory,
        &path,
        Purpose::PreparedSetup,
        prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
    .unwrap();
    PreparedSetupReview::open(
        &path,
        PASSWORD,
        &root.join("prepared-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap()
}
#[test]
fn portable_selected_system_apps_restore_exact_offline_dependency_closure() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fake = fixture(&root);
    let original = fs::read(fake.original.join("repo/flathub.trustedkeys.gpg")).unwrap();
    let (path, expected) = package(&root, &fake, false);
    let setup = setup(&root, expected);
    let mut import = ApplicationImport::review(
        &setup,
        &path,
        PASSWORD,
        &root.join("apps-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(import.manifest().deployments.len(), 2);
    let home = root.join("target-home");
    private(&home);
    assert_eq!(
        import
            .restore(&fake, &home, &AtomicBool::new(false))
            .unwrap(),
        2
    );
    let calls = fake.calls.borrow();
    let installs: Vec<_> = calls
        .iter()
        .filter(|args| args.first().is_some_and(|verb| verb == "install"))
        .collect();
    assert_eq!(installs[0].last().unwrap(), RUNTIME);
    assert_eq!(installs[1].last().unwrap(), APP);
    assert_eq!(
        fs::read(home.join(".local/share/flatpak/repo/restored-object")).unwrap(),
        b"verified offline object"
    );
    assert_eq!(
        fs::read(fake.original.join("repo/flathub.trustedkeys.gpg")).unwrap(),
        original
    );
}
#[test]
fn portable_application_authentication_and_signature_failures_never_install() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let mut fake = fixture(&root);
    let (path, expected) = package(&root, &fake, false);
    let setup = setup(&root, expected);
    assert!(
        ApplicationImport::review(
            &setup,
            &path,
            b"wrong password",
            &root.join("wrong"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    let mut import = ApplicationImport::review(
        &setup,
        &path,
        PASSWORD,
        &root.join("apps-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    fake.fail_signature = true;
    let home = root.join("target-home");
    private(&home);
    assert!(
        import
            .restore(&fake, &home, &AtomicBool::new(false))
            .is_err()
    );
    assert!(fake.installed.borrow().is_empty());
    fs::write(
        root.join("apps-quarantine/home/installation-0/.ostree/repo/signed-object"),
        b"changed object",
    )
    .unwrap();
    fake.calls.borrow_mut().clear();
    assert!(
        import
            .restore(&fake, &home, &AtomicBool::new(false))
            .is_err()
    );
    assert!(fake.calls.borrow().is_empty());
}

pub struct UserSubset<'a>(pub &'a Fake);
impl Runner for UserSubset<'_> {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        let mapped: Vec<_> = args
            .iter()
            .map(|arg| {
                if home == self.0.source_home && arg == "--user" {
                    "--installation=default".into()
                } else {
                    arg.clone()
                }
            })
            .collect();
        self.0.run(home, &mapped).map(|output| {
            if home == self.0.source_home && args.first().is_some_and(|arg| arg == "list") {
                output.replace("\tsystem\t", "\tuser\t")
            } else {
                output
            }
        })
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        self.0.verify_repository(home)
    }
}
impl OfflineRunner for UserSubset<'_> {
    fn ostree(&self, repository: &Path, args: &[String]) -> Result<String, String> {
        self.0.ostree(repository, args)
    }
}

#[test]
fn selected_user_apps_seal_only_selected_data_and_restore_exact_subset_without_source_change() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fake = fixture(&root);
    let runner = UserSubset(&fake);
    let chosen = fake.source_home.join(".var/app/org.example.Paint");
    let excluded = fake.source_home.join(".var/app/org.example.Excluded");
    private(&chosen);
    private(&excluded);
    fs::write(chosen.join("settings"), b"selected real file bytes").unwrap();
    fs::hard_link(chosen.join("settings"), chosen.join("settings-alias")).unwrap();
    fs::write(
        excluded.join("settings"),
        b"excluded bytes remain only on source",
    )
    .unwrap();
    let selected = InstalledRef {
        reference: APP.into(),
        origin: "flathub".into(),
        installation: Installation::User,
        name: "Paint".into(),
    };
    assert!(
        PortableFlatpakSource::review(&runner, &fake.source_home, vec![selected.clone()]).is_err()
    );
    let source =
        PortableFlatpakSource::review_selected_state(&runner, &fake.source_home, vec![selected])
            .unwrap();
    let stage = root.join("selected-stage");
    private(&stage);
    let journals = root.join("selected-journals");
    private(&journals);
    let cancel = AtomicBool::new(false);
    let staged = source
        .stage(
            &runner,
            StageGrant {
                root: &stage,
                journals: &journals,
                allocation_bytes: 64 << 20,
                include_user_state: true,
            },
            &cancel,
        )
        .unwrap();
    assert!(
        stage
            .join("app-state/org.example.Paint/data/settings")
            .exists()
    );
    assert!(!stage.join("app-state/org.example.Excluded").exists());
    assert!(!stage.join(".local/share/flatpak").exists());
    let operation = uuid::Uuid::new_v4().to_string();
    let package = root.join(format!("applications-{operation}.sealed"));
    let expected = staged
        .seal(
            &package,
            &operation,
            "prepared-binding",
            Zeroizing::new(PASSWORD.to_vec()),
            limits(),
            &cancel,
        )
        .unwrap();
    assert_eq!(expected.consumer_protocol, 2);
    assert!(
        crate::setup::applications_admission::require(None, Some(expected.consumer_protocol))
            .is_err()
    );
    let setup = setup(&root, expected.clone());
    let home = root.join("selected-target");
    private(&home);
    for child in [
        ".local",
        ".local/state",
        ".local/state/tilo",
        ".local/state/tilo/portable-applications",
    ] {
        private(&home.join(child));
    }
    let workspace = home
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
    let collision_home = root.join("collision-target");
    private(&collision_home);
    for child in [
        ".local",
        ".local/state",
        ".local/state/tilo",
        ".local/state/tilo/portable-applications",
        ".var",
        ".var/app",
        ".var/app/org.example.Paint",
    ] {
        private(&collision_home.join(child));
    }
    let existing = collision_home.join(".var/app/org.example.Paint/settings");
    fs::write(&existing, b"existing target data must remain").unwrap();
    let collision_workspace = collision_home
        .join(".local/state/tilo/portable-applications")
        .join(&operation);
    private(&collision_workspace);
    let mut collision = ApplicationImport::review(
        &setup,
        &package,
        PASSWORD,
        &collision_workspace.join("imported"),
        &cancel,
    )
    .unwrap();
    assert!(
        collision
            .adopt_user_state(&runner, &collision_home, &collision_workspace, &cancel)
            .is_err()
    );
    assert_eq!(
        fs::read(existing).unwrap(),
        b"existing target data must remain"
    );
    assert!(!collision_workspace.join("selected-data-journals").exists());
    assert!(fake.installed.borrow().is_empty());

    import
        .adopt_user_state(&runner, &home, &workspace, &cancel)
        .unwrap();
    import.restore(&runner, &home, &cancel).unwrap();
    import
        .adopt_user_state(&runner, &home, &workspace, &cancel)
        .unwrap();
    let restored = home.join(".var/app/org.example.Paint");
    assert_eq!(
        fs::read(restored.join("settings")).unwrap(),
        b"selected real file bytes"
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(restored.join("settings")).unwrap().ino(),
        fs::metadata(restored.join("settings-alias")).unwrap().ino()
    );
    assert!(!home.join(".var/app/org.example.Excluded").exists());
    assert_eq!(
        fs::read(excluded.join("settings")).unwrap(),
        b"excluded bytes remain only on source"
    );
    assert_eq!(
        fs::read(chosen.join("settings")).unwrap(),
        b"selected real file bytes"
    );
    assert_eq!(fake.installed.borrow().len(), 2);
    let mut downgraded = expected;
    downgraded.consumer_protocol = 1;
    assert!(
        ApplicationImport::review_expected(
            &downgraded,
            &package,
            PASSWORD,
            &root.join("downgraded-quarantine"),
            &cancel
        )
        .is_err()
    );
}

#[test]
fn selected_app_state_mutation_refuses_without_unselected_data_reads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fake = fixture(&root);
    let selected = InstalledRef {
        reference: APP.into(),
        origin: "flathub".into(),
        installation: Installation::System("default".into()),
        name: "Paint".into(),
    };
    let chosen = fake.source_home.join(".var/app/org.example.Paint");
    private(&chosen);
    fs::write(chosen.join("settings"), b"initial").unwrap();
    // An excluded unreadable/symlink subtree must never become part of the data grant.
    std::os::unix::fs::symlink(
        "/unavailable/excluded-source",
        fake.source_home.join(".var/app/org.example.Excluded"),
    )
    .unwrap();
    let source =
        PortableFlatpakSource::review_selected_state(&fake, &fake.source_home, vec![selected])
            .unwrap();
    fs::write(chosen.join("settings"), b"changed after explicit review").unwrap();
    let stage = root.join("refused-stage");
    private(&stage);
    let journals = root.join("refused-journals");
    private(&journals);
    assert!(
        source
            .stage(
                &fake,
                StageGrant {
                    root: &stage,
                    journals: &journals,
                    allocation_bytes: 64 << 20,
                    include_user_state: true
                },
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(&stage).unwrap().count(), 0);
}

#[test]
fn selected_system_app_data_refuses_new_global_override_before_staging() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let fake = fixture(&root);
    let selected = InstalledRef {
        reference: APP.into(),
        origin: "flathub".into(),
        installation: Installation::System("default".into()),
        name: "Paint".into(),
    };
    let source =
        PortableFlatpakSource::review_selected_state(&fake, &fake.source_home, vec![selected])
            .unwrap();
    private(&fake.source_home.join(".local/share/flatpak/overrides"));
    let override_path = fake
        .source_home
        .join(".local/share/flatpak/overrides/global");
    fs::write(&override_path, b"[Context]\nfilesystems=host;\n").unwrap();
    assert!(source.validate().is_err());
    let stage = root.join("global-refused-stage");
    private(&stage);
    let journals = root.join("global-refused-journals");
    private(&journals);
    assert!(
        source
            .stage(
                &fake,
                StageGrant {
                    root: &stage,
                    journals: &journals,
                    allocation_bytes: 64 << 20,
                    include_user_state: true
                },
                &AtomicBool::new(false)
            )
            .is_err()
    );
    assert_eq!(fs::read_dir(stage).unwrap().count(), 0);
    assert_eq!(
        fs::read(override_path).unwrap(),
        b"[Context]\nfilesystems=host;\n"
    );
}
