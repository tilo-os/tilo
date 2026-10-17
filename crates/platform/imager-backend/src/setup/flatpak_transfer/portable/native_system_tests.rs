//! Explicit privileged qualification: all configuration changes live in a
//! private mount namespace; target migration runs as an actual NSS user.
use super::*;
use crate::setup::flatpak_transfer::{NativeRunner, discover};
use std::ffi::CString;
const TEST: &str = "setup::flatpak_transfer::portable::native_tests::system::real_named_system_producer_and_native_target_restore";
#[path = "native_selected_state_tests.rs"]
mod selected;
const SELECTED_TEST: &str = "setup::flatpak_transfer::portable::native_tests::system::real_named_system_selected_state_restores_without_source_change";
const SELECTED_MODE: &str = "TILO_FLATPAK_SELECTED_STATE_FIXTURE";
fn selected_mode() -> bool {
    std::env::var(SELECTED_MODE).as_deref() == Ok("1")
}
const EXTRA_TEST: &str = "setup::flatpak_transfer::portable::native_tests::system::real_named_system_extra_data_offline_transfer";
fn extra_mode() -> bool {
    std::env::var_os("TILO_FLATPAK_EXTRA_FIXTURE").is_some()
}
fn active_test() -> &'static str {
    if selected_mode() {
        SELECTED_TEST
    } else if extra_mode() {
        EXTRA_TEST
    } else {
        TEST
    }
}
const SCOPE: &str = "native-fixture";
const CHILD: &str = "TILO_FLATPAK_SYSTEM_FIXTURE";

fn nobody() -> (u32, u32) {
    let name = CString::new("nobody").unwrap();
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut result = std::ptr::null_mut();
    // SAFETY: all output buffers remain alive, suitably aligned and bounded.
    let status = unsafe {
        libc::getpwnam_r(
            name.as_ptr(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    assert_eq!(status, 0, "NSS owner lookup failed");
    assert!(
        !result.is_null(),
        "native qualification requires NSS nobody account"
    );
    // SAFETY: successful getpwnam_r initialized this record in our held buffer.
    let record = unsafe { record.assume_init() };
    assert_ne!(record.pw_uid, 0);
    (record.pw_uid, record.pw_gid)
}
fn owner(path: &Path, uid: u32, gid: u32) {
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: fixture-owned exact path is a valid terminated native string.
    assert_eq!(unsafe { libc::chown(path.as_ptr(), uid, gid) }, 0);
}
fn public_directories(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            public_directories(&entry.path());
        }
    }
}
fn required_child(program: &str, args: &[&str]) {
    let result = Command::new(program).args(args).output().unwrap();
    assert!(
        result.status.success(),
        "required native namespace operation failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn namespace_parent() {
    let output = Command::new("/usr/bin/unshare")
        .args(["--mount", "--propagation", "private", "--"])
        .arg(std::env::current_exe().unwrap())
        .args([active_test(), "--exact", "--nocapture"])
        .env(CHILD, "namespace")
        .env("TMPDIR", "/tmp")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native system qualification child failed (namespace creation and nested producer diagnostics follow): {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed; 0 ignored"));
}
fn public_fixture_file(path: &Path, bytes: &[u8]) {
    use std::{
        io::Write as _,
        os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    };
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .unwrap();
    let metadata = file.metadata().unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.nlink(), 1);
    file.write_all(bytes).unwrap();
    file.set_permissions(fs::Permissions::from_mode(0o644))
        .unwrap();
}
fn isolate_configuration() {
    let public: Vec<_> = ["passwd", "group", "nsswitch.conf"]
        .into_iter()
        .map(|name| (name, fs::read(Path::new("/etc").join(name)).unwrap()))
        .collect();
    required_child(
        "/usr/bin/mount",
        &[
            "-t",
            "tmpfs",
            "-o",
            "mode=755,nosuid,nodev",
            "tmpfs",
            "/etc",
        ],
    );
    for (name, bytes) in public {
        public_fixture_file(&Path::new("/etc").join(name), &bytes);
    }
    required_child(
        "/usr/bin/mount",
        &[
            "-t",
            "tmpfs",
            "-o",
            "mode=755,nosuid,nodev",
            "tmpfs",
            "/var/lib",
        ],
    );
    fs::create_dir_all("/etc/flatpak/installations.d").unwrap();
    for path in ["/etc/flatpak", "/etc/flatpak/installations.d"] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
}
fn initialize_signed_source(fixture_root: &Path) -> Agent {
    let gnupg = fixture_root.join("gnupg");
    private(&gnupg);
    let agent = Agent {
        root: fixture_root.to_owned(),
        home: gnupg.clone(),
    };
    let fingerprint = generate_key(fixture_root, &gnupg, "Tilo System Native Fixture");
    let key = command(
        "/usr/bin/gpg",
        fixture_root,
        vec![
            "--homedir".into(),
            gnupg.as_os_str().to_owned(),
            "--export".into(),
            fingerprint.clone().into(),
        ],
    );
    assert!(!key.is_empty());
    public_fixture_file(&fixture_root.join("signing.gpg"), &key);
    let repo = fixture_root.join("origin");
    export_build(fixture_root, &gnupg, &repo, &fingerprint, true);
    export_build_with_extra(
        fixture_root,
        &gnupg,
        &repo,
        &fingerprint,
        false,
        extra_mode(),
    );
    command(
        "/usr/bin/flatpak",
        fixture_root,
        vec![
            "build-update-repo".into(),
            "--collection-id=org.tilo.NativeFixture".into(),
            format!("--gpg-sign={fingerprint}").into(),
            format!("--gpg-homedir={}", gnupg.display()).into(),
            repo.as_os_str().to_owned(),
        ],
    );
    // The private signing-key/GPG namespaces remain root-only. Only the signed
    // source installation and explicit source home are accessible to the user.
    fs::set_permissions(fixture_root, fs::Permissions::from_mode(0o755)).unwrap();
    let system = fixture_root.join("named-system");
    fs::create_dir(&system).unwrap();
    public_fixture_file(
        Path::new("/etc/flatpak/installations.d/native.conf"),
        format!(
            "[Installation \"{SCOPE}\"]\nPath={}\nDisplayName=Native fixture\n",
            system.display()
        )
        .as_bytes(),
    );
    command(
        "/usr/bin/flatpak",
        fixture_root,
        vec![
            "remote-add".into(),
            format!("--installation={SCOPE}").into(),
            format!(
                "--gpg-import={}",
                fixture_root.join("signing.gpg").display()
            )
            .into(),
            "--collection-id=org.tilo.NativeFixture".into(),
            "fixture".into(),
            format!("file://{}", fixture_root.join("origin").display()).into(),
        ],
    );
    command(
        "/usr/bin/flatpak",
        fixture_root,
        strings(&[
            "install",
            &format!("--installation={SCOPE}"),
            "--noninteractive",
            "--assumeyes",
            "fixture",
            RUNTIME,
            APP,
        ]),
    );
    command(
        "/usr/bin/flatpak",
        fixture_root,
        strings(&[
            "remote-modify",
            &format!("--installation={SCOPE}"),
            "--url=https://127.0.0.1:1/offline-fixture/",
            "fixture",
        ]),
    );
    public_directories(&system);
    agent
}
fn namespace_initialize() {
    // Do not trust the test child marker as namespace authority. Even direct
    // invocation must establish a fresh private namespace before any mount.
    // SAFETY: unshare affects only this privileged fixture process.
    assert_eq!(
        unsafe { libc::unshare(libc::CLONE_NEWNS) },
        0,
        "required private mount namespace unavailable"
    );
    let slash = CString::new("/").unwrap();
    // SAFETY: valid terminated path; propagation-only mount has no data pointer.
    assert_eq!(
        unsafe {
            libc::mount(
                std::ptr::null(),
                slash.as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        },
        0,
        "private namespace propagation failed"
    );
    let (uid, gid) = nobody();
    isolate_configuration();
    let temporary = tempfile::tempdir().unwrap();
    let fixture_root = temporary.path().canonicalize().unwrap();
    private(&fixture_root);
    let _agent = initialize_signed_source(&fixture_root);
    let source = fixture_root.join("source-owner");
    private(&source);
    owner(&source, uid, gid);
    if extra_mode() {
        prepare_capacity_target(&fixture_root, uid, gid);
    }
    let output = Command::new("/usr/bin/setpriv")
        .args([
            format!("--reuid={uid}"),
            format!("--regid={gid}"),
            "--clear-groups".into(),
            "--no-new-privs".into(),
            "--".into(),
        ])
        .arg(std::env::current_exe().unwrap())
        .args([active_test(), "--exact", "--nocapture"])
        .env(CHILD, "owner")
        .env("TILO_FLATPAK_SYSTEM_SOURCE", &source)
        .env(
            "TILO_FLATPAK_CAPACITY_TARGET",
            fixture_root.join("capacity-target"),
        )
        .env("TMPDIR", "/tmp")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "native owner producer failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed; 0 ignored"));
}
fn prepare_capacity_target(root: &Path, uid: u32, gid: u32) {
    let target = root.join("capacity-target");
    private(&target);
    required_child(
        "/usr/bin/mount",
        &[
            "-t",
            "tmpfs",
            "-o",
            "size=32m,mode=700,nosuid,nodev",
            "tmpfs",
            target.to_str().unwrap(),
        ],
    );
    owner(&target, uid, gid);
}
fn verify_constrained_restore(
    import: &mut ApplicationImport,
    source: &PortableFlatpakSource,
    cancel: &AtomicBool,
) {
    use std::os::unix::fs::MetadataExt;
    let target = PathBuf::from(std::env::var_os("TILO_FLATPAK_CAPACITY_TARGET").unwrap());
    let sentinel = target.join("sentinel");
    fs::write(&sentinel, b"original constrained target").unwrap();
    let original_file = fs::metadata(&sentinel).unwrap();
    let original_directory = fs::metadata(&target).unwrap();
    let observed = ObservedNative::default();
    let error = import
        .restore(&observed, &target, cancel)
        .unwrap_err()
        .to_string();
    assert!(error.contains("more free space"), "{error}");
    assert_eq!(
        observed
            .extra_data_checks
            .load(std::sync::atomic::Ordering::Relaxed),
        2,
        "actual signed runtime/app metadata was not verified"
    );
    assert_eq!(
        observed.commands.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "capacity refusal began a native target operation"
    );
    assert_eq!(
        observed.installs.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    let after_file = fs::metadata(&sentinel).unwrap();
    let after_directory = fs::metadata(&target).unwrap();
    for (before, after) in [
        (&original_file, &after_file),
        (&original_directory, &after_directory),
    ] {
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.uid(),
                before.gid(),
                before.mode()
            ),
            (
                after.dev(),
                after.ino(),
                after.uid(),
                after.gid(),
                after.mode()
            )
        );
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    }
    assert_eq!(fs::read(&sentinel).unwrap(), b"original constrained target");
    let entries: Vec<_> = fs::read_dir(&target)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![OsString::from("sentinel")]);
    assert!(!target.join(".local").exists());
    source.validate().unwrap();
}
fn owner_producer() {
    // SAFETY: this call merely observes process authority.
    assert_ne!(rustix::process::geteuid().as_raw(), 0);
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
    environment.set("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS", "1");
    let source_home = PathBuf::from(std::env::var_os("TILO_FLATPAK_SYSTEM_SOURCE").unwrap());
    let inventory = discover(&NativeRunner, &source_home).unwrap();
    let scope = Installation::System(SCOPE.into());
    let app = inventory
        .refs
        .iter()
        .find(|item| item.installation == scope && item.reference == APP)
        .unwrap()
        .clone();
    assert!(
        inventory
            .refs
            .iter()
            .any(|item| item.installation == scope && item.reference == RUNTIME)
    );
    assert!(
        !inventory
            .refs
            .iter()
            .any(|item| item.installation == Installation::User)
    );
    crate::setup::qualification_frontend::call(serde_json::json!({
        "operation": "paint-flatpak", "home": source_home,
    }));
    if selected_mode() {
        selected::owner(&source_home, app);
        return;
    }
    let source = PortableFlatpakSource::review(&NativeRunner, &source_home, vec![app]).unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    private(&root);
    let stage = root.join("stage");
    private(&stage);
    let cancel = AtomicBool::new(false);
    let staged = source
        .stage(
            &NativeOfflineRunner,
            StageGrant {
                root: &stage,
                journals: &root.join("journals"),
                allocation_bytes: 256 << 20,
                include_user_state: false,
            },
            &cancel,
        )
        .unwrap();
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
    let setup = prepared_setup(&root, expected, &cancel);
    let mut import =
        ApplicationImport::review(&setup, &package, PASSWORD, &root.join("import"), &cancel)
            .unwrap();
    assert_eq!(import.manifest.deployments.len(), 2);
    assert!(
        import
            .manifest
            .deployments
            .iter()
            .all(|deployment| deployment.installed.installation == scope)
    );
    if extra_mode() {
        verify_constrained_restore(&mut import, &source, &cancel);
    }
    verify_restored(&mut import, &root.join("target"), &source_home, &cancel);
    source.validate().unwrap();
    if extra_mode() {
        verify_extra_refusals(&stage, &root, &cancel);
        source.validate().unwrap();
    }
}
fn verify_restored(
    import: &mut ApplicationImport,
    target: &Path,
    source_home: &Path,
    cancel: &AtomicBool,
) {
    let scope = Installation::System(SCOPE.into());
    private(target);
    private(&target.join(".var/app/org.tilo.Fixture"));
    fs::write(
        target.join(".var/app/org.tilo.Fixture/session"),
        b"independent session",
    )
    .unwrap();
    assert_eq!(
        import
            .restore(&NativeOfflineRunner, target, cancel)
            .unwrap(),
        2
    );
    for reference in [RUNTIME, APP] {
        let original = NativeRunner
            .run(
                source_home,
                &[
                    "info".into(),
                    scope.argument(),
                    "--show-commit".into(),
                    reference.into(),
                ],
            )
            .unwrap();
        assert_eq!(
            native(target, &["info", "--user", "--show-commit", reference]).trim(),
            original.trim()
        );
    }
    NativeOfflineRunner.verify_repository(target).unwrap();
    if extra_mode() {
        let files = target
            .join(".local/share/flatpak/app/org.tilo.Fixture/x86_64/stable/active/files/extra");
        assert_eq!(
            fs::read(files.join("applied.bin")).unwrap(),
            b"original raw proprietary fixture download"
        );
        assert!(
            !files.join("payload.bin").exists(),
            "real sandboxed apply_extra did not transform the raw input"
        );
    }
    assert_eq!(
        fs::read(target.join(".var/app/org.tilo.Fixture/session")).unwrap(),
        b"independent session"
    );
}

fn prepared_setup(root: &Path, expected: Expectation, cancel: &AtomicBool) -> PreparedSetupReview {
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Native system fixture",
        [1, 2, 3],
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .unwrap();
    let setup = root.join("setup");
    private(&setup);
    identity
        .export(
            &setup.join(prepared_setup::IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let mut manifest = Manifest::new(
        "native-system-binding".into(),
        identity.public_key(),
        vec![],
    )
    .unwrap();
    manifest.portable_flatpak = Some(expected);
    manifest.write(&setup).unwrap();
    let package = root.join("setup.sealed");
    ExportReview::review(
        &setup,
        &package,
        Purpose::PreparedSetup,
        prepared_setup::LIMITS,
    )
    .unwrap()
    .export(Zeroizing::new(PASSWORD.to_vec()), cancel)
    .unwrap();
    PreparedSetupReview::open(&package, PASSWORD, &root.join("setup-review"), cancel).unwrap()
}
#[test]
fn real_named_system_producer_and_native_target_restore() {
    match std::env::var(CHILD).as_deref() {
        Ok("owner") => owner_producer(),
        Ok("namespace") => namespace_initialize(),
        _ => {
            // SAFETY: geteuid only reads current process credentials.
            assert_eq!(
                rustix::process::geteuid().as_raw(),
                0,
                "explicit system qualification requires privileged runner"
            );
            namespace_parent();
        }
    }
}

#[test]
fn real_named_system_extra_data_offline_transfer() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_FLATPAK_EXTRA_FIXTURE", "1");
    real_named_system_producer_and_native_target_restore();
}

#[test]
fn real_named_system_selected_state_restores_without_source_change() {
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(SELECTED_MODE, "1");
    environment.remove("TILO_FLATPAK_EXTRA_FIXTURE");
    real_named_system_producer_and_native_target_restore();
}

fn verify_extra_refusals(stage: &Path, root: &Path, cancel: &AtomicBool) {
    use glib::variant::ToVariant;
    let manifest: ApplicationsManifest =
        serde_json::from_slice(&fs::read(stage.join("applications.json")).unwrap()).unwrap();
    let app = manifest
        .deployments
        .iter()
        .find(|item| item.installed.reference == APP)
        .unwrap();
    let (prefix, suffix) = app.commit.split_at(2);
    let detached = stage
        .join(&app.repository)
        .join(".ostree/repo/objects")
        .join(prefix)
        .join(format!("{suffix}.commitmeta"));
    let original = fs::read(&detached).unwrap();
    for missing in [false, true] {
        // Break a possible immutable hardlink before altering the private stage.
        fs::remove_file(&detached).unwrap();
        if !missing {
            let dict = glib::Variant::from_bytes_with_type(
                &glib::Bytes::from_owned(original.clone()),
                glib::VariantTy::new("a{sv}").unwrap(),
            );
            assert!(dict.is_normal_form());
            let mut values = dict
                .get::<std::collections::HashMap<String, glib::Variant>>()
                .unwrap();
            let mut bytes = b"original raw proprietary fixture download".to_vec();
            *bytes.first_mut().unwrap() ^= 1;
            values.insert(
                "xa.extra-data".into(),
                vec![(b"payload.bin\0".to_vec(), bytes)].to_variant(),
            );
            fs::write(&detached, values.to_variant().data()).unwrap();
        }
        let fixture = root.join(if missing {
            "missing-extra"
        } else {
            "corrupt-extra"
        });
        private(&fixture);
        let mut import = repackage_extra(stage, &fixture, cancel);
        let target = fixture.join("target");
        private(&target);
        fs::write(target.join("sentinel"), b"untouched target").unwrap();
        let error = import
            .restore(&NativeOfflineRunner, &target, cancel)
            .unwrap_err()
            .to_string();
        if missing {
            assert!(error.contains("unavailable"), "{error}");
        } else {
            assert!(error.contains("SHA256"), "{error}");
        }
        assert!(
            !target.join(".local").exists(),
            "invalid extra-data began a target repository transaction"
        );
        assert_eq!(
            fs::read(target.join("sentinel")).unwrap(),
            b"untouched target"
        );
        fs::write(&detached, &original).unwrap();
    }
}

fn repackage_extra(stage: &Path, fixture: &Path, cancel: &AtomicBool) -> ApplicationImport {
    let limits = Limits {
        plaintext_bytes: 128 << 20,
        entries: 8192,
        metadata_bytes: 8 << 20,
    };
    let operation = uuid::Uuid::new_v4().to_string();
    let package = fixture.join(format!("applications-{operation}.sealed"));
    let info = ExportReview::review(stage, &package, Purpose::FlatpakApplications, limits)
        .unwrap()
        .export_bound(
            Zeroizing::new(PASSWORD.to_vec()),
            "native-system-binding",
            cancel,
        )
        .unwrap();
    let digest = ImportReview::review(
        &package,
        PASSWORD,
        Purpose::FlatpakApplications,
        limits,
        cancel,
    )
    .unwrap()
    .ciphertext_digest();
    let expected = Expectation {
        schema: 1,
        consumer_protocol: 1,
        operation,
        purpose: Purpose::FlatpakApplications,
        filename: package.file_name().unwrap().to_str().unwrap().into(),
        setup_binding: "native-system-binding".into(),
        sealed_bytes: info.sealed_bytes,
        plaintext_bytes: info.plaintext_bytes,
        entries: info.entries,
        ciphertext_sha256: digest,
        manifest_sha256: Sha256::digest(fs::read(stage.join("applications.json")).unwrap()).into(),
        limits,
    };
    let setup = prepared_setup(fixture, expected, cancel);
    ApplicationImport::review(&setup, &package, PASSWORD, &fixture.join("import"), cancel).unwrap()
}

/// Initialize only the expressly isolated configuration supplied by the existing
/// GPT/UDisks coordinator. Do not fork its mount topology away from its daemon.
pub fn initialize_original_source(root: &Path) -> Agent {
    use std::os::unix::fs::MetadataExt as _;
    assert_eq!(rustix::process::geteuid().as_raw(), 0);
    let marker = Path::new("/etc/.tilo-paired-qualification");
    let metadata = fs::symlink_metadata(marker).expect("isolated coordinator /etc marker required");
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), 0);
    assert_eq!(metadata.mode() & 0o7777, 0o600);
    assert_eq!(fs::read(marker).unwrap(), b"disposable-paired-config-only");
    let mounts = fs::read_to_string("/proc/self/mountinfo").unwrap();
    assert!(
        mounts.lines().any(|row| {
            let Some((fields, filesystem)) = row.split_once(" - ") else {
                return false;
            };
            fields.split_whitespace().nth(4) == Some("/etc")
                && filesystem.split_whitespace().next() == Some("tmpfs")
        }),
        "coordinator must retain actual private tmpfs /etc alongside UDisks namespace"
    );
    let configuration = Path::new("/etc/flatpak/installations.d");
    assert!(
        fs::read_dir(configuration).unwrap().next().is_none(),
        "qualification must not adopt ambient named installation configuration"
    );
    initialize_signed_source(root)
}
