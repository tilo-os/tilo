use super::*;
use crate::setup::{
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose},
    prepared_identity::PreparedIdentity,
    prepared_setup::{self, Manifest},
};
use browser_continuity::{PLAN_FILE, PROFILES_DIRECTORY, Plan, Profile, Provider};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt};

const PASSWORD: &[u8] = b"several unrelated installation words";
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
    handoff: BrowserHandoff,
}
fn browser_carrier(root: &Path, plan: Plan, delivered: &Path) -> (PathBuf, BrowserHandoff) {
    let source = root.join("browser-source");
    directory(&source);
    directory(&source.join(PROFILES_DIRECTORY));
    let profile = source.join(PROFILES_DIRECTORY).join(&plan.profiles[0].id);
    directory(&profile);
    let plan_bytes = plan.encode().unwrap();
    file(&source.join(PLAN_FILE), &plan_bytes);
    file(&profile.join("Bookmarks"), b"original private bookmarks");
    let operation = uuid::Uuid::new_v4().to_string();
    let package = delivered.join(format!("browser-{operation}.sealed"));
    let limits = Limits {
        plaintext_bytes: 1024 * 1024,
        entries: 32,
        metadata_bytes: 64 * 1024,
    };
    let cancel = AtomicBool::new(false);
    let info = ExportReview::review(&source, &package, Purpose::BrowserProfiles, limits)
        .unwrap()
        .export_bound(
            Zeroizing::new(PASSWORD.to_vec()),
            &plan.setup_binding,
            &cancel,
        )
        .unwrap();
    let digest = ImportReview::review(
        &package,
        PASSWORD,
        Purpose::BrowserProfiles,
        limits,
        &cancel,
    )
    .unwrap()
    .ciphertext_digest();
    let payload = Expectation {
        schema: 1,
        operation,
        purpose: Purpose::BrowserProfiles,
        filename: package.file_name().unwrap().to_str().unwrap().into(),
        setup_binding: plan.setup_binding.clone(),
        sealed_bytes: info.sealed_bytes,
        plaintext_bytes: info.plaintext_bytes,
        entries: info.entries,
        ciphertext_sha256: digest,
        plan_sha256: Sha256::digest(plan_bytes).into(),
        limits,
    };
    (package, BrowserHandoff { plan, payload })
}
fn provision_device(
    root: &Path,
    identity: &PreparedIdentity,
    environment: &mut tilo_test_env::ScopedEnv,
) {
    let config = root.join("config");
    directory(&config);
    environment.set("XDG_CONFIG_HOME", &config);
    environment.set("TILO_IDENTITY_ROOT", root.join("system"));
    let record = identity.custody_directory().join("primary.json");
    let mut registry = tilo_identity_host::registry::IdentityRegistry::default();
    let id = registry.register_record(&record).unwrap().id.clone();
    tilo_identity_host::registry::IdentityRegistryStore::from_env()
        .unwrap()
        .save(&registry)
        .unwrap();
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let cert_path =
        tilo_identity_host::provisioning::ensure_device_certificate_for_record(&record, &paths)
            .unwrap();
    let cert = tilo_user_identity::persist::read_device_cert(&cert_path).unwrap();
    tilo_identity_host::device_fleet::DeviceFleetStore::from_env()
        .unwrap()
        .remember_certificate(&id, cert)
        .unwrap();
}
impl Fixture {
    fn new() -> Self {
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().canonicalize().unwrap();
        directory(&root);
        let home = root.join("target-home");
        directory(&home);
        let delivered = home.join(".local/state/tilo/prepared-browser");
        directory(&delivered);
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [1, 2, 3],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        provision_device(&root, &identity, &mut environment);
        let plan = Plan::new(
            "prepared-binding".into(),
            identity.public_key(),
            vec![
                Profile::new(
                    "a".repeat(64),
                    Provider::Chromium,
                    vec![".config".into(), "chromium".into(), "Default".into()],
                    true,
                    false,
                )
                .unwrap(),
            ],
        )
        .unwrap();
        let (package, handoff) = browser_carrier(&root, plan, &delivered);
        let receipt = delivered.join("expectation.json");
        file(&receipt, &serde_json::to_vec(&handoff.payload).unwrap());
        let stage = root.join("setup-stage");
        directory(&stage);
        identity
            .export(
                &stage.join(prepared_setup::IDENTITY_FILE),
                Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            )
            .unwrap();
        let mut manifest =
            Manifest::new("prepared-binding".into(), identity.public_key(), vec![]).unwrap();
        manifest.browser = Some(handoff.clone());
        manifest.write(&stage).unwrap();
        let setup = root.join("setup.sealed");
        ExportReview::review(
            &stage,
            &setup,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
        let workspace = home
            .join(".local/state/tilo/browser-continuity")
            .join(&handoff.payload.operation);
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
            handoff,
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
    fn activate(&self, attempt: &str) -> serde_json::Value {
        let mut output = Vec::new();
        run(
            self.args("activate", attempt),
            &mut &PASSWORD[..],
            &mut output,
        )
        .unwrap();
        serde_json::from_slice(&output).unwrap()
    }
}
#[test]
fn canonical_headless_activation_and_retry_verify_original_ciphertext_and_output() {
    let fixture = Fixture::new();
    fs::set_permissions(&fixture.home, fs::Permissions::from_mode(0o755)).unwrap();
    let cipher = fs::read(&fixture.package).unwrap();
    let first = fixture.activate("first");
    assert_eq!(first["verified"], true);
    assert_eq!(first["event"], "complete");
    let completion = browser_continuity::completion::Completion::read(
        &fixture
            .workspace
            .join(browser_continuity::completion::FILE_NAME),
    )
    .unwrap();
    let setup_digest = ImportReview::review(
        &fixture.setup,
        PASSWORD,
        Purpose::PreparedSetup,
        prepared_setup::LIMITS,
        &AtomicBool::new(false),
    )
    .unwrap()
    .ciphertext_digest();
    completion
        .verify_binding(
            &fixture.handoff.plan,
            setup_digest,
            fixture.handoff.payload.ciphertext_sha256,
            &fixture.handoff.payload.operation,
        )
        .unwrap();
    let cert: tilo_user_identity::DeviceCert =
        tilo_user_identity::canonical::from_canonical_bytes(completion.certificate()).unwrap();
    tilo_user_identity::verify_device_cert(&cert).unwrap();
    tilo_identity_host::certificates::authorize_join("", &cert).unwrap();
    assert_eq!(
        completion
            .verify_projection(cert.device_public_key)
            .unwrap()
            .to_str(),
        first["projection"].as_str()
    );
    let projection = PathBuf::from(first["projection"].as_str().unwrap());
    let profile = projection
        .join(PROFILES_DIRECTORY)
        .join(&fixture.handoff.plan.profiles[0].id);
    assert_eq!(
        fs::read(profile.join("Bookmarks")).unwrap(),
        b"original private bookmarks"
    );
    assert!(!profile.join("Cookies").exists());
    assert!(!profile.join("History").exists());
    assert_eq!(fixture.activate("retry"), first);
    assert_eq!(fs::read(&fixture.package).unwrap(), cipher);
    let mut output = Vec::new();
    let original_time = fs::metadata(profile.join("Bookmarks"))
        .unwrap()
        .modified()
        .unwrap();
    file(&profile.join("Bookmarks"), b"tampered bookmarks");
    assert!(
        run(
            fixture.args("activate", "tampered-output"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(
        output.is_empty(),
        "an existing receipt must not authorize changed bytes"
    );
    file(&profile.join("Bookmarks"), b"original private bookmarks");
    fs::File::open(profile.join("Bookmarks"))
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(original_time))
        .unwrap();
    assert_eq!(fixture.activate("repaired"), first);
    file(&profile.join("Cookies"), b"unselected credentials");
    assert!(
        run(
            fixture.args("activate", "extra-file"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    fs::remove_file(profile.join("Cookies")).unwrap();
    let mut changed_cipher = cipher;
    let index = changed_cipher.len() - 1;
    changed_cipher[index] ^= 1;
    file(&fixture.package, &changed_cipher);
    assert!(
        run(
            fixture.args("activate", "tampered-cipher"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
}
#[test]
fn wrong_password_and_swapped_declaration_do_not_activate_or_emit_private_data() {
    let fixture = Fixture::new();
    let mut output = Vec::new();
    assert!(
        run(
            fixture.args("activate", "wrong-password"),
            &mut &b"wrong password"[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(fs::read_dir(&fixture.workspace).unwrap().count(), 0);
    let mut swapped = fixture.handoff.payload.clone();
    swapped.ciphertext_sha256[0] ^= 1;
    file(&fixture.receipt, &serde_json::to_vec(&swapped).unwrap());
    assert!(
        run(
            fixture.args("activate", "wrong-declaration"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(fs::read_dir(&fixture.workspace).unwrap().count(), 0);
}
#[test]
fn forged_completed_receipt_and_replaced_output_directory_are_not_completion() {
    let fixture = Fixture::new();
    let first = fixture.activate("first");
    let projection = PathBuf::from(first["projection"].as_str().unwrap());
    let moved = projection.parent().unwrap().join("original");
    fs::rename(&projection, &moved).unwrap();
    directory(&projection);
    let mut output = Vec::new();
    assert!(
        run(
            fixture.args("activate", "replaced-directory"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    fs::remove_dir(&projection).unwrap();
    fs::rename(&moved, &projection).unwrap();
    let receipt_path = fixture.workspace.join(RECEIPT);
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    receipt["handoff"]["plan"]["identity_public_key"][0] = serde_json::json!(255);
    file(&receipt_path, &serde_json::to_vec(&receipt).unwrap());
    assert!(
        run(
            fixture.args("activate", "forged-identity"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
}
#[test]
fn strict_arguments_and_bounded_secret_input_fail_before_any_output() {
    let fixture = Fixture::new();
    let mut output = Vec::new();
    let mut args = fixture.args("activate", "bad-arg");
    args.extend([OsString::from("--password"), OsString::from("secret")]);
    assert!(run(args, &mut &PASSWORD[..], &mut output).is_err());
    assert!(
        run(
            fixture.args("activate", "oversized"),
            &mut &vec![b'x'; 4097][..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert_eq!(fs::read_dir(&fixture.workspace).unwrap().count(), 0);
}

#[test]
fn prepared_identity_import_returns_the_authenticated_browser_declaration() {
    let fixture = Fixture::new();
    let args = [
        ("--package", fixture.setup.clone()),
        ("--quarantine", fixture.root.join("identity-import")),
        ("--identity-dir", fixture.root.join("target-identities")),
        ("--config-root", fixture.root.join("target-config")),
    ]
    .into_iter()
    .flat_map(|(key, value)| [OsString::from(key), value.into_os_string()]);
    let mut output = Vec::new();
    crate::setup::prepared_setup_helper::run(args, &mut &PASSWORD[..], &mut output).unwrap();
    let receipt: crate::setup::prepared_setup_helper::Receipt =
        serde_json::from_slice(&output).unwrap();
    assert_eq!(
        receipt.identity_public_key,
        fixture.handoff.plan.identity_public_key
    );
    assert_eq!(receipt.browser, Some(fixture.handoff));
    assert_eq!(receipt.account_count, 0);
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("original private bookmarks")
    );
}

#[test]
fn missing_certified_device_custody_cannot_publish_success_or_a_completion_signature() {
    let fixture = Fixture::new();
    let paths = tilo_identity_host::provisioning::collaboration_identity_paths();
    let key = fs::read(&paths.device_privkey).unwrap();
    fs::remove_file(&paths.device_privkey).unwrap();
    let mut output = Vec::new();
    assert!(
        run(
            fixture.args("activate", "missing-device"),
            &mut &PASSWORD[..],
            &mut output
        )
        .is_err()
    );
    assert!(output.is_empty());
    assert!(
        !fixture
            .workspace
            .join(browser_continuity::completion::FILE_NAME)
            .exists()
    );
    file(&paths.device_privkey, &key);
    assert_eq!(fixture.activate("restored-device")["event"], "complete");
}
