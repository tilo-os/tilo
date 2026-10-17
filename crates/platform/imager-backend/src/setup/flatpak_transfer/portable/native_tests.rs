//! Real consumer qualification. The original installation is an isolated USER
//! fixture: these tests do not qualify native SYSTEM discovery or privileged media.
use super::*;
use crate::setup::{
    migration::PinnedDirectory,
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest, PreparedSetupReview},
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString, fs, os::unix::fs::PermissionsExt, process::Command, sync::atomic::AtomicBool,
    time::Duration,
};
use zeroize::Zeroizing;
mod runnable;

const PASSWORD: &[u8] = b"a native offline fixture password";
const APP: &str = "app/org.tilo.Fixture/x86_64/stable";
const RUNTIME: &str = "runtime/org.tilo.FixtureRuntime/x86_64/stable";
const TEST: &str = "setup::flatpak_transfer::portable::native_tests::real_signed_offline_target_restores_and_rejects_invalid_authority";
fn private(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn command(program: &str, home: &Path, args: impl IntoIterator<Item = OsString>) -> Vec<u8> {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
        .env("HOME", home)
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(
            "XDG_CACHE_HOME",
            home.join(".local/share/flatpak/.migration-cache"),
        )
        .env("FLATPAK_USER_DIR", home.join(".local/share/flatpak"))
        .args(args);
    let mut process =
        bounded_command::spawn_prepared(command, None, 8 * 1024 * 1024, Duration::from_secs(120))
            .unwrap();
    loop {
        if let Some(result) = process.try_wait() {
            let output = result.unwrap();
            assert!(
                output.status.success(),
                "{program} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return output.stdout;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn native(home: &Path, values: &[&str]) -> String {
    NativeOfflineRunner
        .run(
            home,
            &values.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap()
}
#[derive(Default)]
pub struct ObservedNative {
    installs: std::sync::atomic::AtomicUsize,
    commands: std::sync::atomic::AtomicUsize,
    extra_data_checks: std::sync::atomic::AtomicUsize,
}
impl ObservedNative {
    #[cfg(feature = "native-capture-qualification")]
    pub fn install_count(&self) -> usize {
        self.installs.load(std::sync::atomic::Ordering::Relaxed)
    }
}
impl Runner for ObservedNative {
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        self.commands
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if arguments.first().is_some_and(|verb| verb == "install") {
            self.installs
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        NativeOfflineRunner.run(home, arguments)
    }
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        self.commands
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        NativeOfflineRunner.verify_repository(home)
    }
}
impl OfflineRunner for ObservedNative {
    fn verify_extra_data(&self, repository: &Path, hash: &str) -> Result<u64, String> {
        self.extra_data_checks
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        NativeOfflineRunner.verify_extra_data(repository, hash)
    }
    fn ostree(&self, repository: &Path, arguments: &[String]) -> Result<String, String> {
        self.commands
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        NativeOfflineRunner.ostree(repository, arguments)
    }
}
pub struct Agent {
    root: PathBuf,
    home: PathBuf,
}
impl Drop for Agent {
    fn drop(&mut self) {
        command(
            "/usr/bin/gpgconf",
            &self.root,
            vec![
                "--homedir".into(),
                self.home.as_os_str().to_owned(),
                "--kill".into(),
                "gpg-agent".into(),
            ],
        );
    }
}
struct Fixture {
    _agent: Agent,
    _environment: tilo_test_env::ScopedEnv,
    _temporary: tempfile::TempDir,
    root: PathBuf,
    gnupg: PathBuf,
    home: PathBuf,
    stage: PathBuf,
    manifest: ApplicationsManifest,
    app_commit: String,
}
/// Explicit live material for the authenticated selected-app receiver constructor.
/// Retain this owner until the original source epoch and signing tools retire.
#[cfg(feature = "native-capture-qualification")]
pub struct RunnableFixture(Fixture);
#[cfg(feature = "native-capture-qualification")]
impl RunnableFixture {
    /// Install through the same actual native CLI into the admitted disposable
    /// GPT Home before capture. This material builder supplies no source grant.
    pub fn create_in(actual_home: &Path) -> Self {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
        assert!(actual_home.is_absolute());
        assert_eq!(actual_home.canonicalize().unwrap(), actual_home);
        let held = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(actual_home)
            .unwrap();
        let before = held.metadata().unwrap();
        // SAFETY: observe current native original account, never impersonate it.
        let uid = rustix::process::geteuid().as_raw();
        assert_ne!(uid, 0, "runnable USER installation needs original account");
        assert!(before.is_dir() && before.uid() == uid && before.mode() & 0o7022 == 0);
        validate_fixture_branches(actual_home, uid);
        // Only a fresh disposable installation branch is admitted. Never mutate
        // an existing user repository/remotes or follow its internal links.
        assert_eq!(
            fs::symlink_metadata(actual_home.join(".local/share/flatpak"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::NotFound
        );
        let fixture = Fixture::create_variant_at(true, Some(actual_home));
        validate_fixture_branches(actual_home, uid);
        let after = fs::symlink_metadata(actual_home).unwrap();
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
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.uid(),
                before.gid(),
                before.mode()
            ),
            (
                held.metadata().unwrap().dev(),
                held.metadata().unwrap().ino(),
                held.metadata().unwrap().uid(),
                held.metadata().unwrap().gid(),
                held.metadata().unwrap().mode()
            )
        );
        let result = Self::from_fixture(fixture);
        validate_fixture_branches(actual_home, uid);
        let after = fs::symlink_metadata(actual_home).unwrap();
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
        result
    }
    fn from_fixture(fixture: Fixture) -> Self {
        for path in [
            fixture.home.join("owner-document"),
            fixture.home.join(".var/app/org.tilo.Excluded"),
            fixture
                .home
                .join(".var/app/org.tilo.Fixture/data/selected-document"),
        ] {
            assert_eq!(
                fs::symlink_metadata(path).unwrap_err().kind(),
                std::io::ErrorKind::NotFound
            );
        }
        let data = fixture.home.join(".var/app/org.tilo.Fixture/data");
        private(&data);
        fs::write(
            data.join("selected-document"),
            b"genuine selected application state",
        )
        .unwrap();
        fs::set_permissions(
            data.join("selected-document"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let excluded = fixture.home.join(".var/app/org.tilo.Excluded/data");
        private(&excluded);
        fs::write(
            excluded.join("document"),
            b"unselected private application state",
        )
        .unwrap();
        fs::write(
            fixture.home.join("owner-document"),
            b"declined main Home document",
        )
        .unwrap();
        Self(fixture)
    }
    pub fn source_home(&self) -> &Path {
        &self.0.home
    }
}
fn validate_fixture_branches(home: &Path, uid: u32) {
    use std::os::unix::fs::MetadataExt as _;
    for relative in [
        ".local",
        ".local/share",
        ".local/share/flatpak",
        ".var",
        ".var/app",
        ".var/app/org.tilo.Fixture",
        ".var/app/org.tilo.Fixture/data",
        ".var/app/org.tilo.Excluded",
        ".var/app/org.tilo.Excluded/data",
    ] {
        match fs::symlink_metadata(home.join(relative)) {
            Ok(m) => assert!(
                m.is_dir() && m.uid() == uid && m.mode() & 0o7022 == 0,
                "existing fixture branch must remain inside admitted Home"
            ),
            Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        }
    }
}
impl Fixture {
    fn create() -> Self {
        Self::create_variant(false)
    }
    fn create_variant(runnable: bool) -> Self {
        Self::create_variant_at(runnable, None)
    }
    fn create_variant_at(runnable: bool, actual_home: Option<&Path>) -> Self {
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        environment.set("TILO_NATIVE_FLATPAK_FIXTURE_DIAGNOSTICS", "1");
        for tool in [
            "/usr/bin/flatpak",
            "/usr/bin/ostree",
            "/usr/bin/gpg",
            "/usr/bin/gpgconf",
        ] {
            assert!(
                Path::new(tool).is_file(),
                "required native verification tool missing: {tool}"
            );
        }
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        private(&root);
        let gnupg = root.join("gnupg");
        private(&gnupg);
        let agent = Agent {
            root: root.clone(),
            home: gnupg.clone(),
        };
        let fingerprint = generate_key(&root, &gnupg, "Tilo Native Fixture");
        let key = command(
            "/usr/bin/gpg",
            &root,
            vec![
                "--homedir".into(),
                gnupg.as_os_str().to_owned(),
                "--export".into(),
                fingerprint.clone().into(),
            ],
        );
        assert!(!key.is_empty());
        fs::write(root.join("signing.gpg"), &key).unwrap();
        let repo = root.join("origin");
        if runnable {
            export_build_material(&root, &gnupg, &repo, &fingerprint, true, false, true);
            export_build_material(&root, &gnupg, &repo, &fingerprint, false, false, true);
        } else {
            export_build(&root, &gnupg, &repo, &fingerprint, true);
            export_build(&root, &gnupg, &repo, &fingerprint, false);
        }
        command(
            "/usr/bin/flatpak",
            &root,
            vec![
                "build-update-repo".into(),
                "--collection-id=org.tilo.NativeFixture".into(),
                format!("--gpg-sign={fingerprint}").into(),
                format!("--gpg-homedir={}", gnupg.display()).into(),
                repo.as_os_str().to_owned(),
            ],
        );
        let (home, stage, manifest, app_commit) = if let Some(home) = actual_home {
            create_installation_at(&root, home.to_path_buf())
        } else {
            create_installation(&root)
        };
        Self {
            _agent: agent,
            _environment: environment,
            _temporary: temporary,
            root,
            gnupg,
            home,
            stage,
            manifest,
            app_commit,
        }
    }
    fn import(&self, label: &str, manifest: &ApplicationsManifest) -> (ApplicationImport, PathBuf) {
        let manifest = manifest.clone();
        manifest.validate().unwrap();
        let bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(self.stage.join("applications.json"), &bytes).unwrap();
        let limits = Limits {
            plaintext_bytes: 64 * 1024 * 1024,
            entries: 8192,
            metadata_bytes: 8 * 1024 * 1024,
        };
        let operation = uuid::Uuid::new_v4().to_string();
        let package = self.root.join(format!("applications-{operation}.sealed"));
        let cancel = AtomicBool::new(false);
        let info =
            ExportReview::review(&self.stage, &package, Purpose::FlatpakApplications, limits)
                .unwrap()
                .export_bound(Zeroizing::new(PASSWORD.to_vec()), "native-binding", &cancel)
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
            consumer_protocol: 1,
            operation,
            purpose: Purpose::FlatpakApplications,
            filename: package.file_name().unwrap().to_str().unwrap().into(),
            setup_binding: "native-binding".into(),
            sealed_bytes: info.sealed_bytes,
            plaintext_bytes: info.plaintext_bytes,
            entries: info.entries,
            ciphertext_sha256: digest,
            manifest_sha256: Sha256::digest(&bytes).into(),
            limits,
        };
        let identity = PreparedIdentity::create(
            &self.root.join(format!("identity-{label}")),
            "Native Fixture",
            [1, 2, 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let setup_root = self.root.join(format!("setup-{label}"));
        private(&setup_root);
        identity
            .export(
                &setup_root.join(prepared_setup::IDENTITY_FILE),
                Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            )
            .unwrap();
        let mut setup_manifest =
            Manifest::new("native-binding".into(), identity.public_key(), vec![]).unwrap();
        setup_manifest.portable_flatpak = Some(expected);
        setup_manifest.write(&setup_root).unwrap();
        let setup_package = self.root.join(format!("setup-{label}.sealed"));
        ExportReview::review(
            &setup_root,
            &setup_package,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
        let setup = PreparedSetupReview::open(
            &setup_package,
            PASSWORD,
            &self.root.join(format!("setup-review-{label}")),
            &cancel,
        )
        .unwrap();
        let target = self.root.join(format!("target-{label}"));
        private(&target);
        private(&target.join(".var/app/org.tilo.Fixture"));
        fs::write(
            target.join(".var/app/org.tilo.Fixture/session"),
            b"independent private app session",
        )
        .unwrap();
        let import = ApplicationImport::review(
            &setup,
            &package,
            PASSWORD,
            &self.root.join(format!("import-{label}")),
            &cancel,
        )
        .unwrap();
        (import, target)
    }
    fn object(&self, suffix: &str) -> PathBuf {
        self.stage
            .join("installation-0/.ostree/repo/objects")
            .join(self.app_commit.get(..2).unwrap())
            .join(format!("{}.{}", self.app_commit.get(2..).unwrap(), suffix))
    }
}
fn create_installation(root: &Path) -> (PathBuf, PathBuf, ApplicationsManifest, String) {
    let home = root.join("source-user");
    private(&home);
    create_installation_at(root, home)
}
fn create_installation_at(
    root: &Path,
    home: PathBuf,
) -> (PathBuf, PathBuf, ApplicationsManifest, String) {
    add_source_remote(root, &home);
    for reference in [RUNTIME, APP] {
        native(
            &home,
            &[
                "install",
                "--user",
                "--noninteractive",
                "--assumeyes",
                "--no-deps",
                "--no-related",
                "fixture",
                reference,
            ],
        );
    }
    native(
        &home,
        &[
            "remote-modify",
            "--user",
            "--url=https://127.0.0.1:1/offline-fixture/",
            "fixture",
        ],
    );
    let stage = root.join("package-source");
    private(&stage);
    private(&stage.join("installation-0"));
    super::super::export_sideload(
        &NativeOfflineRunner,
        &home,
        &Installation::User,
        &[APP.into(), RUNTIME.into()],
        &stage.join("installation-0"),
    )
    .unwrap();
    let source = Remote {
        installation: Installation::System("default".into()),
        name: "fixture".into(),
        url: "https://127.0.0.1:1/offline-fixture/".into(),
        options: String::new(),
    };
    let definition =
        super::super::extract_remote_definition(&source, &home.join(".local/share/flatpak"))
            .unwrap();
    fs::write(stage.join("approved.flatpakrepo"), &definition).unwrap();
    fs::write(
        stage.join("installation-0/.ostree/repo/fixture.trustedkeys.gpg"),
        fs::read(home.join(".local/share/flatpak/repo/fixture.trustedkeys.gpg")).unwrap(),
    )
    .unwrap();
    let deployments: [Deployment; 2] = [APP, RUNTIME].map(|reference| Deployment {
        installed: InstalledRef {
            reference: reference.into(),
            origin: "fixture".into(),
            installation: source.installation.clone(),
            name: "Fixture".into(),
        },
        commit: native(&home, &["info", "--user", "--show-commit", reference])
            .trim()
            .into(),
        repository: "installation-0".into(),
        remote: "tilo-portable-0".into(),
    });
    let app_commit = deployments.first().unwrap().commit.clone();
    let manifest = ApplicationsManifest {
        schema: 1,
        selected: vec![deployments.first().unwrap().installed.clone()],
        deployments: deployments.to_vec(),
        trust: vec![Trust {
            remote: "tilo-portable-0".into(),
            definition: "approved.flatpakrepo".into(),
            sha256: Sha256::digest(&definition).into(),
            source,
        }],
        user_state: false,
        app_state: Vec::new(),
    };
    (home, stage, manifest, app_commit)
}
fn add_source_remote(root: &Path, home: &Path) {
    let url = reqwest::Url::from_directory_path(root.join("origin"))
        .unwrap()
        .to_string();
    let key = STANDARD.encode(fs::read(root.join("signing.gpg")).unwrap());
    let definition = root.join("source.flatpakrepo");
    fs::write(
        &definition,
        format!("[Flatpak Repo]\nTitle=Native Fixture\nUrl={url}\nGPGKey={key}\n"),
    )
    .unwrap();
    // Source construction uses the real bounded CLI so failures expose native
    // diagnostics. Destination restoration still uses NativeOfflineRunner's
    // canonical trust gate and isolated user installation.
    command(
        "/usr/bin/flatpak",
        home,
        strings(&[
            "remote-add",
            "--user",
            "--from",
            "--collection-id=org.tilo.NativeFixture",
            // Upstream remote-add checks an explicit collection's --gpg-import
            // before reading .flatpakrepo (1.18.4, lines 300-315). Supply the
            // same genuine key embedded in the definition, never disable GPG.
            &format!("--gpg-import={}", root.join("signing.gpg").display()),
            "fixture",
            definition.to_str().unwrap(),
        ]),
    );
}
fn generate_key(root: &Path, gnupg: &Path, name: &str) -> String {
    command(
        "/usr/bin/gpg",
        root,
        vec![
            "--homedir".into(),
            gnupg.as_os_str().to_owned(),
            "--batch".into(),
            "--pinentry-mode".into(),
            "loopback".into(),
            "--passphrase".into(),
            "".into(),
            "--quick-generate-key".into(),
            name.into(),
            "ed25519".into(),
            "sign".into(),
            "0".into(),
        ],
    );
    let listing = command(
        "/usr/bin/gpg",
        root,
        vec![
            "--homedir".into(),
            gnupg.as_os_str().to_owned(),
            "--with-colons".into(),
            "--list-secret-keys".into(),
            name.into(),
        ],
    );
    String::from_utf8(listing)
        .unwrap()
        .lines()
        .find(|line| line.starts_with("fpr:"))
        .unwrap()
        .split(':')
        .nth(9)
        .unwrap()
        .to_owned()
}
fn export_build(root: &Path, gnupg: &Path, repo: &Path, key: &str, runtime: bool) {
    export_build_with_extra(root, gnupg, repo, key, runtime, false);
}
fn export_build_with_extra(
    root: &Path,
    gnupg: &Path,
    repo: &Path,
    key: &str,
    runtime: bool,
    extra: bool,
) {
    export_build_material(root, gnupg, repo, key, runtime, extra, false);
}
fn export_build_material(
    root: &Path,
    gnupg: &Path,
    repo: &Path,
    key: &str,
    runtime: bool,
    extra: bool,
    runnable: bool,
) {
    let name = if runtime {
        "runtime-build"
    } else {
        "app-build"
    };
    let build = root.join(name);
    // Flatpak 1.18.4 build-export first requires files + metadata for BOTH
    // kinds, then writes runtime usr into the committed files tree (upstream
    // app/flatpak-builtins-build-export.c:823-827,945-950). This bootstrap
    // fixture needs no downloaded SDK and never launches its payload.
    private(&build.join("files"));
    let contents = build.join(if runtime { "usr" } else { "files" });
    private(&contents.join("bin"));
    private(&build.join("export"));
    let metadata = if runtime {
        "[Runtime]\nname=org.tilo.FixtureRuntime\nruntime=org.tilo.FixtureRuntime/x86_64/stable\nsdk=org.tilo.FixtureRuntime/x86_64/stable\n"
    } else {
        "[Application]\nname=org.tilo.Fixture\nruntime=org.tilo.FixtureRuntime/x86_64/stable\nsdk=org.tilo.FixtureRuntime/x86_64/stable\ncommand=fixture\n"
    };
    let metadata = if extra {
        let payload = b"original raw proprietary fixture download";
        let digest = hex::encode(Sha256::digest(payload));
        let cache = root.join(".local/share/flatpak/extra-data").join(&digest);
        private(&cache);
        fs::write(cache.join("payload.bin"), payload).unwrap();
        fs::write(contents.join("bin/apply_extra"), extra_apply_program()).unwrap();
        fs::set_permissions(
            contents.join("bin/apply_extra"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        format!(
            "{metadata}\n[Extra Data]\nuri0=https://127.0.0.1:1/never-fetch\nname0=payload.bin\nchecksum0={digest}\nsize0={}\ninstalled-size0=67108864\n",
            payload.len()
        )
    } else {
        metadata.to_owned()
    };
    fs::write(build.join("metadata"), metadata).unwrap();
    if runnable {
        assert!(
            !extra,
            "runnable fixture is distinct from extra-data fixture"
        );
        runnable::populate(&contents, runtime);
        if !runtime {
            private(&build.join("export/share/applications"));
            fs::write(
                build.join("export/share/applications/org.tilo.Fixture.desktop"),
                "[Desktop Entry]\nType=Application\nName=Tilo Runnable Fixture\nExec=fixture\nTerminal=false\n",
            ).unwrap();
        }
    } else {
        fs::write(contents.join("bin/fixture"), b"#!/bin/sh\nexit 0\n").unwrap();
    }
    fs::set_permissions(
        contents.join("bin/fixture"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let mut args = strings(&["build-export", "--collection-id=org.tilo.NativeFixture"]);
    if runtime {
        args.push("--runtime".into());
    }
    args.extend([
        format!("--gpg-sign={key}").into(),
        format!("--gpg-homedir={}", gnupg.display()).into(),
        repo.as_os_str().to_owned(),
        build.as_os_str().to_owned(),
        "stable".into(),
    ]);
    command("/usr/bin/flatpak", root, args);
}

// Static x86_64 fixture apply_extra: rename payload.bin to applied.bin, exit
// with syscall status. No host interpreter/library/SDK or network is involved.
fn extra_apply_program() -> Vec<u8> {
    let code = [
        0xb8, 82, 0, 0, 0, 0x48, 0x8d, 0x3d, 18, 0, 0, 0, 0x48, 0x8d, 0x35, 23, 0, 0, 0, 0x0f,
        0x05, 0x89, 0xc7, 0xb8, 60, 0, 0, 0, 0x0f, 0x05,
    ];
    let mut result = b"\x7fELF\x02\x01\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
    result.extend_from_slice(&2u16.to_le_bytes());
    result.extend_from_slice(&62u16.to_le_bytes());
    result.extend_from_slice(&1u32.to_le_bytes());
    result.extend_from_slice(&0x400078u64.to_le_bytes());
    result.extend_from_slice(&64u64.to_le_bytes());
    result.extend_from_slice(&0u64.to_le_bytes());
    result.extend_from_slice(&0u32.to_le_bytes());
    for field in [64u16, 56, 1, 0, 0, 0] {
        result.extend_from_slice(&field.to_le_bytes());
    }
    result.extend_from_slice(&1u32.to_le_bytes());
    result.extend_from_slice(&5u32.to_le_bytes());
    let length = 120 + code.len() as u64 + 24;
    for field in [0u64, 0x400000, 0x400000, length, length, 4096] {
        result.extend_from_slice(&field.to_le_bytes());
    }
    assert_eq!(result.len(), 120);
    result.extend_from_slice(&code);
    result.extend_from_slice(b"payload.bin\0applied.bin\0");
    assert_eq!(result.len() as u64, length);
    result
}

fn assert_undeployed(home: &Path) {
    for reference in [APP, RUNTIME] {
        assert!(
            NativeOfflineRunner
                .run(home, &["info".into(), "--user".into(), reference.into()])
                .is_err()
        );
    }
}
#[test]
fn real_signed_offline_target_restores_and_rejects_invalid_authority() {
    // Root cannot own a production user installation. Re-exec only this test
    // under an unrelated unprivileged UID; every fixture is created by that owner.
    // SAFETY: geteuid has no preconditions and only reads current credentials.
    if rustix::process::geteuid().as_raw() == 0 {
        assert!(
            std::env::var_os("TILO_NATIVE_FLATPAK_OWNER_CHILD").is_none(),
            "test failed to drop root authority"
        );
        assert!(
            Path::new("/usr/bin/setpriv").is_file(),
            "required isolated owner tool missing"
        );
        let output = Command::new("/usr/bin/setpriv")
            .args([
                "--reuid=65534",
                "--regid=65534",
                "--clear-groups",
                "--no-new-privs",
                "--",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([TEST, "--exact", "--nocapture"])
            .env("TILO_NATIVE_FLATPAK_OWNER_CHILD", "1")
            // The container's root-only TMPDIR is not a grant to the owner child.
            // tempfile creates each exclusive owner-private fixture beneath /tmp.
            .env("TMPDIR", "/tmp")
            .env_remove("TMP")
            .env_remove("TEMP")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native owner fixture failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let report = String::from_utf8(output.stdout).unwrap();
        assert!(
            report.contains("1 passed; 0 failed; 0 ignored"),
            "native child did not execute the exact required test: {report}"
        );
        return;
    }
    let fixture = Fixture::create();
    let cancel = AtomicBool::new(false);
    verify_positive(&fixture, &cancel);
    verify_corruption(&fixture, &cancel);
    verify_missing_signature(&fixture, &cancel);
    verify_wrong_key(&fixture, &cancel);
    assert_eq!(
        native(&fixture.home, &["info", "--user", "--show-commit", APP]).trim(),
        fixture.app_commit
    );
    PinnedDirectory::open_absolute(&fixture.home)
        .unwrap()
        .validate()
        .unwrap();
}

fn verify_positive(fixture: &Fixture, cancel: &AtomicBool) {
    let (mut import, target) = fixture.import("valid", &fixture.manifest);
    assert_eq!(
        import
            .restore(&NativeOfflineRunner, &target, cancel)
            .unwrap(),
        2
    );
    verify_target(fixture, &target);
    NativeOfflineRunner.verify_repository(&target).unwrap();
    let observed = ObservedNative::default();
    // The API reports verified deployments, including already restored refs.
    // Observe real delegated effects separately: retry must not install again.
    assert_eq!(import.restore(&observed, &target, cancel).unwrap(), 2);
    assert_eq!(
        observed.installs.load(std::sync::atomic::Ordering::Relaxed),
        0
    );
    verify_target(fixture, &target);
    // Real GC aliases work above; a differently named alias pointing at the
    // same correctly signed runtime must still fail the logical binding proof.
    let runtime = fixture
        .manifest
        .deployments
        .iter()
        .find(|deployment| deployment.installed.reference == RUNTIME)
        .unwrap();
    NativeOfflineRunner
        .ostree(
            &target.join(".local/share/flatpak/repo"),
            &[
                "refs".into(),
                "--create=deploy/app/org.tilo.Impostor/x86_64/stable".into(),
                runtime.commit.clone(),
            ],
        )
        .unwrap();
    assert_eq!(
        NativeOfflineRunner.verify_repository(&target).unwrap_err(),
        "Repository commit is not bound to its logical ref"
    );
}
fn verify_target(fixture: &Fixture, target: &Path) {
    for deployment in &fixture.manifest.deployments {
        assert_eq!(
            native(
                target,
                &[
                    "info",
                    "--user",
                    "--show-commit",
                    &deployment.installed.reference
                ]
            )
            .trim(),
            deployment.commit
        );
        assert_eq!(
            native(
                target,
                &[
                    "info",
                    "--user",
                    "--show-origin",
                    &deployment.installed.reference
                ]
            )
            .trim(),
            "tilo-portable-0"
        );
    }
    assert_eq!(
        fs::read(target.join(".var/app/org.tilo.Fixture/session")).unwrap(),
        b"independent private app session"
    );
    assert!(
        target
            .join(
                ".local/share/flatpak/app/org.tilo.Fixture/x86_64/stable/active/files/bin/fixture"
            )
            .is_file()
    );
    assert_eq!(
        fs::read(target.join(
            ".local/share/flatpak/app/org.tilo.Fixture/x86_64/stable/active/files/bin/fixture"
        ))
        .unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    assert_eq!(fs::read(target.join(".local/share/flatpak/runtime/org.tilo.FixtureRuntime/x86_64/stable/active/files/bin/fixture")).unwrap(),b"#!/bin/sh\nexit 0\n");
}

fn verify_corruption(fixture: &Fixture, cancel: &AtomicBool) {
    let object = fixture.object("commit");
    let original = fs::read(&object).unwrap();
    // create-usb can share immutable object inodes: replace only this fixture
    // namespace entry instead of modifying a possible original hardlink.
    fs::remove_file(&object).unwrap();
    fs::write(&object, b"corrupted real commit object").unwrap();
    let (mut corrupt, target) = fixture.import("corrupt", &fixture.manifest);
    assert!(
        corrupt
            .restore(&NativeOfflineRunner, &target, cancel)
            .is_err()
    );
    assert!(
        NativeOfflineRunner
            .run(&target, &["info".into(), "--user".into(), APP.into()])
            .is_err()
    );
    assert_eq!(
        native(&target, &["info", "--user", "--show-commit", RUNTIME]).trim(),
        fixture
            .manifest
            .deployments
            .iter()
            .find(|item| item.installed.reference == RUNTIME)
            .unwrap()
            .commit
    );
    fs::remove_file(&object).unwrap();
    fs::write(&object, original).unwrap();
}

fn verify_missing_signature(fixture: &Fixture, cancel: &AtomicBool) {
    let signature = fixture.object("commitmeta");
    let original = fs::read(&signature).unwrap();
    fs::remove_file(&signature).unwrap();
    let (mut unsigned, target) = fixture.import("unsigned", &fixture.manifest);
    assert!(
        unsigned
            .restore(&NativeOfflineRunner, &target, cancel)
            .is_err()
    );
    assert!(
        NativeOfflineRunner
            .run(&target, &["info".into(), "--user".into(), APP.into()])
            .is_err()
    );
    fs::write(&signature, original).unwrap();
}

fn verify_wrong_key(fixture: &Fixture, cancel: &AtomicBool) {
    let wrong = generate_key(&fixture.root, &fixture.gnupg, "Tilo Wrong Fixture");
    let key = command(
        "/usr/bin/gpg",
        &fixture.root,
        vec![
            "--homedir".into(),
            fixture.gnupg.as_os_str().to_owned(),
            "--export".into(),
            wrong.into(),
        ],
    );
    fs::write(
        fixture
            .stage
            .join("installation-0/.ostree/repo/fixture.trustedkeys.gpg"),
        key,
    )
    .unwrap();
    let definition = super::super::extract_remote_definition(
        &fixture.manifest.trust.first().unwrap().source,
        &fixture.stage.join("installation-0/.ostree"),
    )
    .unwrap();
    fs::write(fixture.stage.join("approved.flatpakrepo"), &definition).unwrap();
    let mut manifest = fixture.manifest.clone();
    manifest.trust.first_mut().unwrap().sha256 = Sha256::digest(&definition).into();
    let (mut wrong, target) = fixture.import("wrong-key", &manifest);
    assert!(
        wrong
            .restore(&NativeOfflineRunner, &target, cancel)
            .is_err()
    );
    assert_undeployed(&target);
}

#[cfg(feature = "native-capture-qualification")]
#[path = "native_system_tests.rs"]
pub mod system;

#[test]
fn runnable_existing_home_alias_refuses_before_native_installation() {
    use std::os::unix::fs::{MetadataExt as _, symlink};
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("home");
    let outside = temporary.path().join("outside");
    private(&home);
    private(&outside);
    fs::write(outside.join("untouched"), b"outside source authority").unwrap();
    let before = fs::symlink_metadata(&outside).unwrap();
    // SAFETY: actual owner of these generated disposable directories.
    let uid = rustix::process::geteuid().as_raw();
    for edge in [".local", ".var"] {
        symlink(&outside, home.join(edge)).unwrap();
        assert!(std::panic::catch_unwind(|| validate_fixture_branches(&home, uid)).is_err());
        assert_eq!(
            fs::read(outside.join("untouched")).unwrap(),
            b"outside source authority"
        );
        let after = fs::symlink_metadata(&outside).unwrap();
        assert_eq!(
            (
                before.dev(),
                before.ino(),
                before.mode(),
                before.uid(),
                before.gid()
            ),
            (
                after.dev(),
                after.ino(),
                after.mode(),
                after.uid(),
                after.gid()
            )
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
        fs::remove_file(home.join(edge)).unwrap();
    }
}
