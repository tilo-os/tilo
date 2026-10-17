//! The paired positive requires genuine root admission and actual unprivileged
//! consumers. It intentionally fails while the release Flatpak gate remains shut.
use super::*;
use crate::setup::{
    flatpak_transfer::{
        HandoffInventory, Installation, NativeRunner, discover,
        portable::{ApplicationImport, NativeOfflineRunner, PortableFlatpakSource},
    },
    media_setup::{PreparedSetupDelivery, boot_medium::with_boot_medium, protected_output},
    migration_handoff::{self, Categories, Plan},
    portable_home_import::{HomeImportGrants, PortableHomeReview},
    portable_home_media::Expectation as HomeExpectation,
    prepared_setup::completion::{self, boot_review::CompletedBootReview},
};
use std::{
    os::unix::fs::OpenOptionsExt as _,
    process::{Command, Stdio},
};
const TEST: &str = "setup::home_capture::store::original::native_tests::output::signed_original_home_and_named_applications_complete_and_restore";
const FIRSTUSE_TEST: &str = "setup::capture_release_native::current_candidate_firstuse_consumes_home_apps_accounts_and_opens_files";
fn firstuse() -> bool {
    std::env::var("TILO_ORIGINAL_PAIRED_FIRSTUSE").as_deref() == Ok("1")
}
const MOUNTED_TEST: &str = "setup::home_capture::store::original::native_tests::output::signed_original_mounted_home_and_named_applications_complete_and_restore";
fn mounted() -> bool {
    std::env::var("TILO_ORIGINAL_PAIRED_MOUNTED").as_deref() == Ok("1")
}
pub(super) const APPLICATIONS_TEST: &str = "setup::home_capture::store::original::native_tests::output::signed_original_named_applications_without_home_complete_and_restore";
const USER_APPLICATIONS_TEST: &str = "setup::home_capture::store::original::native_tests::output::signed_original_selected_user_applications_without_home_complete_and_restore";
pub(super) fn user_applications() -> bool {
    std::env::var("TILO_ORIGINAL_PAIRED_USER_APPLICATIONS").as_deref() == Ok("1")
}
pub(super) fn applications_only() -> bool {
    std::env::var("TILO_ORIGINAL_PAIRED_APPLICATIONS_ONLY").as_deref() == Ok("1")
}
pub(super) fn guided_story() -> bool {
    std::env::var("TILO_ORIGINAL_PAIRED_GUIDED_STORY").as_deref() == Ok("1")
}
const ROLE: &str = "TILO_ORIGINAL_PAIRED_ROLE";
const APP: &str = "app/org.tilo.Fixture/x86_64/stable";
const RUNTIME: &str = "runtime/org.tilo.FixtureRuntime/x86_64/stable";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Captured {
    pub(super) plan: Plan,
    pub(super) reservation: u64,
    pub(super) preparation: Option<crate::setup::migration_review::OriginalPreparation>,
}
pub(super) fn child_dispatch() -> bool {
    let Some(role) = std::env::var_os(ROLE) else {
        return false;
    };
    assert_eq!(
        rustix::process::geteuid().as_raw(),
        1000,
        "actual source/target owner required"
    );
    assert!(
        matches!(
            role.to_str(),
            Some("capture" | "restore" | "restore-applications")
        ),
        "unknown qualifier role"
    );
    match role.to_str() {
        Some("capture") => capture_child(),
        Some("restore") => restore_child(),
        Some("restore-applications") => super::paired_apps::restore_child(),
        _ => return false,
    }
    true
}
pub(super) fn owner_child(role: &str, root: &Path) -> Command {
    let mut child = Command::new("/usr/bin/setpriv");
    child
        .args([
            "--reuid=1000",
            "--regid=1001",
            "--groups=1002,1003",
            "--no-new-privs",
            "--",
        ])
        .arg("/proc/self/exe")
        .args([
            if firstuse() {
                FIRSTUSE_TEST
            } else if user_applications() {
                if std::env::var("TILO_ORIGINAL_PAIRED_INSTALLED_APPS").as_deref() == Ok("1") {
                    "setup::home_capture::store::original::native_tests::output::installed_original6_user_applications_candidate_qualifies_and_reattests_rootlessly"
                } else { USER_APPLICATIONS_TEST }
            } else if applications_only() {
                APPLICATIONS_TEST
            } else if mounted() {
                MOUNTED_TEST
            } else {
                TEST
            },
            "--exact",
            "--nocapture",
        ])
        .env(ROLE, role)
        .env("TILO_ORIGINAL_PAIRED_ROOT", root)
        .env("TMPDIR", "/tmp")
        .env(runtime_identity::FILE_SECRETS_ENV, "1")
        .env_remove("TILO_FLATPAK_EXTRA_FIXTURE");
    child
}
pub(super) fn require_child(output: std::process::Output) {
    assert!(
        output.status.success(),
        "actual unprivileged qualifier failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed; 0 ignored"));
}
fn child_root() -> PathBuf {
    PathBuf::from(std::env::var_os("TILO_ORIGINAL_PAIRED_ROOT").unwrap())
}
pub(super) fn capture(root: &Path, home: &Path, operation: &str) -> Captured {
    let destination = root.join("capture");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    own_source(&destination);
    let output = owner_child("capture", root)
        .env("TILO_ORIGINAL_PAIRED_HOME", home)
        .env("TILO_ORIGINAL_PAIRED_OPERATION", operation)
        .output()
        .unwrap();
    require_child(output);
    let path = destination.join("captured.json");
    let metadata = fs::symlink_metadata(&path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.uid(), 1000);
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert!(metadata.len() <= 9 * 1024 * 1024);
    let captured: Captured = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    captured.plan.validate().unwrap();
    let actual = migration_handoff::capture(home, operation, captured.plan.categories).unwrap();
    assert_eq!(
        actual.home, captured.plan.home,
        "capture must bind the actual supplied source inode/GPT volume"
    );
    assert_eq!(captured.plan.operation_id, operation);
    captured
}
fn capture_child() {
    let root = child_root();
    let home = PathBuf::from(std::env::var_os("TILO_ORIGINAL_PAIRED_HOME").unwrap());
    let operation = std::env::var("TILO_ORIGINAL_PAIRED_OPERATION").unwrap();
    let preparation = if guided_story() {
        let endpoint = firstuse().then(|| {
            let endpoint =
                reqwest::Url::parse(&std::env::var("TILO_ORIGINAL_PAIRED_DAV_URL").unwrap())
                    .unwrap();
            assert_eq!(endpoint.host_str(), Some("127.0.0.1"));
            endpoint
        });
        Some(
            serde_json::from_value(crate::setup::qualification_frontend::call(
                serde_json::json!({
                    "operation": "prepare-story",
                    "home": home,
                    "destination": root.join("capture/guided-setup.sealed"),
                    "password": std::str::from_utf8(PASSWORD).unwrap(),
                    "endpoint": endpoint,
                }),
            ))
            .unwrap(),
        )
    } else {
        None
    };
    if user_applications() {
        use crate::setup::flatpak_transfer::Runner as _;
        assert!(applications_only());
        // The owner installs actual signed synthetic refs into its real USER
        // installation before review. Root-owned signing secrets stay outside
        // the caller's Home; only the exported public key is consumed.
        let fixture = root.join("flatpak-fixture");
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        let definition = root.join("capture/source.flatpakrepo");
        let key = STANDARD.encode(fs::read(fixture.join("signing.gpg")).unwrap());
        let url = reqwest::Url::from_directory_path(fixture.join("origin")).unwrap();
        fs::write(
            &definition,
            format!("[Flatpak Repo]\nTitle=Native Fixture\nUrl={url}\nGPGKey={key}\n"),
        )
        .unwrap();
        NativeOfflineRunner
            .run(
                &home,
                &[
                    "remote-add".into(),
                    "--user".into(),
                    "--from".into(),
                    format!("--gpg-import={}", fixture.join("signing.gpg").display()),
                    "--collection-id=org.tilo.NativeFixture".into(),
                    "fixture".into(),
                    definition.to_str().unwrap().into(),
                ],
            )
            .unwrap();
        for reference in [RUNTIME, APP] {
            NativeOfflineRunner
                .run(
                    &home,
                    &[
                        "install".into(),
                        "--user".into(),
                        "--noninteractive".into(),
                        "--assumeyes".into(),
                        "--no-deps".into(),
                        "--no-related".into(),
                        "fixture".into(),
                        reference.into(),
                    ],
                )
                .unwrap();
        }
        NativeOfflineRunner
            .run(
                &home,
                &[
                    "remote-modify".into(),
                    "--user".into(),
                    "--url=https://127.0.0.1:1/offline-fixture/".into(),
                    "fixture".into(),
                ],
            )
            .unwrap();
    }
    let inventory = discover(&NativeRunner, &home).unwrap();
    let scope = if user_applications() {
        Installation::User
    } else {
        Installation::System("native-fixture".into())
    };
    let selected = inventory
        .refs
        .iter()
        .find(|r| r.installation == scope && r.reference == APP)
        .expect("genuine named signed app must be discovered")
        .clone();
    assert!(
        inventory
            .refs
            .iter()
            .any(|r| r.installation == scope && r.reference == RUNTIME)
    );
    let source = if applications_only() {
        PortableFlatpakSource::review_selected_state(&NativeRunner, &home, vec![selected.clone()])
    } else {
        PortableFlatpakSource::review(&NativeRunner, &home, vec![selected.clone()])
    }
    .unwrap();
    let requirement = source.storage_requirement(applications_only()).unwrap();
    // Mounted Home deliberately defers content inventory to the authenticated
    // readonly worker. This is a hard fixture storage allowance, not fabricated
    // entry/statistics evidence; actual ExportOriginal enforces the reservation.
    let home_bound = if applications_only() {
        0
    } else if mounted() {
        16 * 1024 * 1024
    } else {
        let pin = PinnedDirectory::open_absolute(&home).unwrap();
        let inventory =
            crate::setup::migration_review::SourceInventory::capture_home(&pin).unwrap();
        let entries = u64::try_from(inventory.entry_count()).unwrap();
        let metadata = entries
            .checked_mul(1024 * 1024)
            .unwrap()
            .min(512 * 1024 * 1024);
        let plaintext_bytes = crate::setup::portable_transfer::stored_bytes(&pin, &inventory)
            .unwrap()
            .checked_add(entries.checked_mul(2048).unwrap())
            .unwrap()
            .checked_add(metadata)
            .unwrap()
            .checked_add(10 * 1024)
            .unwrap();
        crate::setup::portable_transfer::Limits {
            plaintext_bytes,
            entries: inventory.entry_count(),
            metadata_bytes: 1,
        }
        .sealed_bound()
        .unwrap()
    };
    let reservation = requirement
        .staging_bytes
        .checked_mul(2)
        .and_then(|n| n.checked_add(requirement.sealed_bytes))
        .and_then(|n| n.checked_add(home_bound))
        .and_then(|n| n.checked_add(128 * 1024))
        .expect("checked coexistence budget");
    let categories = Categories {
        home: !applications_only(),
        flatpak: true,
        accounts: guided_story(),
    };
    let bind = |path: &Path| {
        migration_handoff::capture(
            path,
            &operation,
            Categories {
                home: false,
                flatpak: true,
                accounts: false,
            },
        )
        .and_then(|p| p.bytes())
        .map_err(|e| e.to_string())
    };
    let retained = if applications_only() {
        HandoffInventory::capture_selected_state(&NativeRunner, &home, vec![selected], bind)
    } else {
        HandoffInventory::capture(&NativeRunner, &home, vec![selected], bind)
    }
    .unwrap();
    if user_applications() {
        assert!(
            retained
                .selected()
                .iter()
                .all(|item| item.installation == Installation::User)
        );
    }
    assert_eq!(
        retained.selected().len(),
        2,
        "actual app/runtime closure required"
    );
    let mut plan = migration_handoff::capture(&home, &operation, categories).unwrap();
    plan.flatpak_inventory = Some(serde_json::from_slice(&retained.bytes().unwrap()).unwrap());
    let captured = Captured {
        plan,
        reservation,
        preparation,
    };
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(root.join("capture/captured.json"))
        .unwrap();
    output
        .write_all(&serde_json::to_vec(&captured).unwrap())
        .unwrap();
    output.sync_all().unwrap();
    source.validate().unwrap();
}
pub(super) fn publish_pending(
    target: &crate::imager_model::TargetDrive,
    raw: &File,
    binding: &crate::media_layout::MediaPartitionBinding,
    full: &Plan,
    reservation: u64,
    package: &Path,
    mounted_geometry: Option<Vec<Vec<u8>>>,
) {
    let cancel = AtomicBool::new(false);
    let mut delivery = PreparedSetupDelivery::review(package, PASSWORD, &cancel).unwrap();
    let mut geometry = full.clone();
    geometry.flatpak_inventory = None;
    let mut source_plans = vec![geometry.bytes().unwrap()];
    if let Some(plans) = mounted_geometry {
        source_plans.extend(plans);
    }
    let inventory = HandoffInventory::parse(
        &serde_json::to_vec(full.flatpak_inventory.as_ref().unwrap()).unwrap(),
    )
    .unwrap();
    for (_, plan) in inventory.sources() {
        source_plans.push(plan.to_vec());
    }
    assert!(
        source_plans.len() >= 2,
        "selected named root must survive raw admission"
    );
    let extent = binding
        .last_lba
        .checked_sub(binding.first_lba)
        .unwrap()
        .checked_add(1)
        .unwrap()
        .checked_mul(binding.sector_bytes)
        .unwrap();
    let conservative_free = block_devices::migration_ext4::available_bytes(extent).unwrap();
    assert!(
        conservative_free
            >= reservation
                .checked_add(delivery.info().sealed_bytes)
                .unwrap(),
        "owned output subject is too small for actual reviewed Home/stage/apps coexistence"
    );
    let layout = crate::setup::OfflineLayout {
        binding: binding.clone(),
        reserved_bytes: reservation,
        setup: delivery.expectation(),
        source_plans,
        target_number: raw.metadata().unwrap().rdev(),
    };
    protected_output::provision_after_write(target, &layout, 1000, &|| false).unwrap();
    protected_output::with_reviewed_inbox(target, raw, &layout, 1000, &cancel, |inbox, _| {
        let name = std::ffi::CString::new(crate::setup::media_setup::DIRECTORY).unwrap();
        // Fresh directory only, below the actual root-retained recipient inbox.
        // This creates no grant or Verify token and never adopts an existing name.
        assert_eq!(
            unsafe { libc::mkdirat(inbox.as_raw_fd(), name.as_ptr(), 0o700) },
            0
        );
        let mut directory_options = cap_primitives::fs::OpenOptions::new();
        use cap_fs_ext::OpenOptionsFollowExt as _;
        directory_options
            .read(true)
            .follow(cap_primitives::fs::FollowSymlinks::No);
        let private = cap_primitives::fs::open(
            &inbox,
            Path::new(crate::setup::media_setup::DIRECTORY),
            &directory_options,
        )?;
        assert!(private.metadata()?.is_dir());
        assert_eq!(unsafe { libc::fchown(private.as_raw_fd(), 1000, 1001) }, 0);
        let mut options = cap_primitives::fs::OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = cap_primitives::fs::open(
            &private,
            Path::new(crate::setup::media_setup::FILE),
            &options,
        )?;
        assert_eq!(unsafe { libc::fchmod(file.as_raw_fd(), 0o600) }, 0);
        assert_eq!(unsafe { libc::fchown(file.as_raw_fd(), 1000, 1001) }, 0);
        delivery.copy_preparation_ciphertext(&mut file, &cancel)?;
        let mut receipt = cap_primitives::fs::open(&private, Path::new("receipt.json"), &options)?;
        assert_eq!(unsafe { libc::fchmod(receipt.as_raw_fd(), 0o600) }, 0);
        assert_eq!(unsafe { libc::fchown(receipt.as_raw_fd(), 1000, 1001) }, 0);
        receipt.write_all(&serde_json::to_vec(&layout.setup)?)?;
        receipt.sync_all()?;
        crate::setup::migration_durability::sync_directory(&private)?;
        Ok(())
    })
    .unwrap();
}

pub struct Committed {
    home: HomeExpectation,
    applications: crate::setup::flatpak_transfer::portable::Expectation,
    key: [u8; 32],
    account_ids: Vec<String>,
}
pub(super) fn complete(
    setup: &PreparedSetupReview,
    transaction: &Arc<OfflineOutputTransaction>,
    outcome: &crate::setup::home_capture::offline_output::OriginalExportOutcome,
) -> Committed {
    let cancel = AtomicBool::new(false);
    let verified = transaction
        .validate_verified_outcome(outcome)
        .expect("only genuine independent verifier completion can mint authority");
    let applications = verified
        .applications()
        .expect("selected system closure must not disappear")
        .clone();
    assert_ne!(applications.operation, verified.expected().operation);
    assert_eq!(applications.setup_binding, setup.manifest().setup_binding);
    let mut completed = setup
        .reseal_completed_home(&verified, Zeroizing::new(PASSWORD.to_vec()), &cancel)
        .unwrap();
    assert!(completed.manifest().offline_home.is_none());
    assert_eq!(
        completed.manifest().portable_flatpak.as_ref(),
        Some(&applications)
    );
    assert_eq!(
        completed.manifest().identity_public_key,
        setup.manifest().identity_public_key
    );
    assert_eq!(
        completed.manifest().account_ids,
        setup.manifest().account_ids
    );
    let published = completed.publish(&verified, &cancel).unwrap();
    assert!(published.setup.valid());
    // Publication reopens ciphertext and authenticates unchanged inner identity.
    completed.validate_origin(&verified).unwrap();
    Committed {
        home: verified.expected().clone(),
        applications,
        key: setup.manifest().identity_public_key,
        account_ids: setup.manifest().account_ids.clone(),
    }
}
pub type CompletedReceiver = fn(
    &crate::setup::media_setup::boot_medium::BootMedium,
    &crate::setup::prepared_setup::completion::boot_review::AuthenticatedCompletedSetup,
    &Committed,
    &Path,
) -> std::io::Result<()>;

pub(super) fn reopen_and_receive(
    target: &crate::imager_model::TargetDrive,
    raw: &File,
    committed: &Committed,
    scratch: &Path,
    receiver: Option<CompletedReceiver>,
) {
    let cancel = AtomicBool::new(false);
    let destination = scratch.join("target-consumer");
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();
    // The fixture's private namespace is established before source initialization.
    // Do not replace a running native installer's state in that namespace.
    assert!(
        !Path::new("/run/tilo-installer").exists(),
        "private fixture must supply an empty installer runtime namespace"
    );
    fs::create_dir("/run/tilo-installer").unwrap();
    let result = Command::new("/usr/bin/mount")
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
    require_tool(result, "isolated installer runtime mount");
    with_boot_medium(target, raw, &cancel, |medium| {
        let review = CompletedBootReview::review(medium, PASSWORD, &cancel)?;
        let capsule = review.retained_setup(&cancel)?;
        assert_eq!(
            capsule
                .authenticated_setup()
                .manifest()
                .portable_flatpak
                .as_ref(),
            Some(&committed.applications)
        );
        assert_eq!(
            capsule.authenticated_setup().manifest().identity_public_key,
            committed.key
        );
        assert_eq!(
            capsule.authenticated_setup().manifest().account_ids,
            committed.account_ids
        );
        assert_eq!(
            capsule
                .authenticated_setup()
                .manifest()
                .completed_home
                .as_ref(),
            Some(&committed.home)
        );
        review.adopt_install_generation(&cancel)?;
        if let Some(receive) = receiver {
            // No metadata-only completion and no detached capsule: the actual
            // readonly BootMedium stays borrowed for the whole first-use route.
            receive(medium, &capsule, committed, scratch)?;
            capsule.validate()?;
        } else {
            fs::copy(capsule.package_path(), destination.join("setup.sealed"))?;
        }
        Ok(())
    })
    .unwrap();
    if receiver.is_some() {
        return;
    }
    assert!(
        completion::selected_generation()
            .unwrap()
            .starts_with("/run/tilo-installer")
    );
    for (kind, filename, digest) in [
        (
            "home",
            committed.home.filename.as_str(),
            committed.home.ciphertext_sha256,
        ),
        (
            "applications",
            committed.applications.filename.as_str(),
            committed.applications.ciphertext_sha256,
        ),
    ] {
        let path = destination.join(filename);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        completion::stream_payload(kind, &mut file, &cancel).unwrap();
        file.sync_all().unwrap();
        use sha2::Digest as _;
        assert_eq!(
            <[u8; 32]>::from(sha2::Sha256::digest(fs::read(&path).unwrap())),
            digest
        );
    }
    let public = serde_json::json!({"home":committed.home,"applications":committed.applications,"key":committed.key,"account_ids":committed.account_ids});
    fs::write(
        destination.join("expected.json"),
        serde_json::to_vec(&public).unwrap(),
    )
    .unwrap();
    for entry in fs::read_dir(&destination).unwrap() {
        fs::set_permissions(entry.unwrap().path(), fs::Permissions::from_mode(0o600)).unwrap();
    }
    own_source(&destination);
    // Root scratch is normally 0700. Grant only traversal of this explicitly
    // disposable fixture ancestor; never normalize original Home or system refs.
    fs::set_permissions(scratch, fs::Permissions::from_mode(0o711)).unwrap();
    let mut child = owner_child("restore", &destination)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(PASSWORD).unwrap();
    require_child(child.wait_with_output().unwrap());
}
fn require_tool(output: std::process::Output, name: &str) {
    assert!(
        output.status.success(),
        "required {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn restore_child() {
    let root = child_root();
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
        &root.join("authenticated-setup"),
        &cancel,
    )
    .unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("expected.json")).unwrap()).unwrap();
    let home_expected: HomeExpectation = serde_json::from_value(expected["home"].clone()).unwrap();
    let applications: crate::setup::flatpak_transfer::portable::Expectation =
        serde_json::from_value(expected["applications"].clone()).unwrap();
    assert_eq!(
        setup.manifest().completed_home.as_ref(),
        Some(&home_expected)
    );
    assert_eq!(
        setup.manifest().portable_flatpak.as_ref(),
        Some(&applications)
    );
    let home = root.join("home");
    for path in [
        home.clone(),
        home.join(".config"),
        home.join(".config/tilo"),
        home.join(".local"),
        home.join(".local/state"),
        home.join(".local/state/tilo"),
    ] {
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let identity_path = setup
        .import_identity(&home.join(".config/tilo"), &password)
        .unwrap();
    if guided_story() {
        assert_eq!(setup.manifest().account_ids.len(), 2);
        assert_eq!(
            serde_json::to_value(&setup.manifest().account_ids).unwrap(),
            expected["account_ids"]
        );
        let signer = runtime_identity::load_from_path(identity_path.clone()).unwrap();
        assert_eq!(
            serde_json::to_value(signer.group_public_key()).unwrap(),
            expected["key"]
        );
        assert_eq!(
            runtime_identity::load_metadata_from_path(&identity_path)
                .unwrap()
                .display_name,
            "Ada"
        );
        let imported = setup
            .import_accounts(&home.join(".config/tilo"), &password)
            .unwrap();
        assert_eq!(
            imported
                .iter()
                .map(|id| id.as_str().to_owned())
                .collect::<Vec<_>>(),
            setup.manifest().account_ids
        );
    }
    let workspace = home.join(".local/state/tilo/portable-home");
    fs::create_dir_all(&workspace).unwrap();
    fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
    let mut home_review = PortableHomeReview::review(
        HomeImportGrants {
            package: root.join(&home_expected.filename),
            home: home.clone(),
            workspace,
        },
        &setup,
        &home_expected,
        &password,
        &cancel,
    )
    .unwrap();
    let preview = home_review
        .prepare(Zeroizing::new(password.to_vec()), &cancel)
        .unwrap();
    assert_eq!(preview.collisions, 0);
    let mut worker = home_review.start_adoption(&cancel).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let result = loop {
        if let Some(result) = worker.try_next() {
            break result.unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "actual home adoption timed out"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(result.complete && result.verified && !result.cancelled);
    assert_eq!(result.failures, 0);
    home_review.verify_completed_adoption(&cancel).unwrap();
    if guided_story() {
        verify_guided_home(&home);
    }
    let mut apps = ApplicationImport::review(
        &setup,
        &root.join(&applications.filename),
        &password,
        &root.join("authenticated-applications"),
        &cancel,
    )
    .unwrap();
    let apps_workspace = home.join(".local/state/tilo/portable-applications");
    fs::create_dir_all(&apps_workspace).unwrap();
    fs::set_permissions(&apps_workspace, fs::Permissions::from_mode(0o700)).unwrap();
    apps.verify_home_for_applications(
        &mut home_review,
        &NativeOfflineRunner,
        &home,
        &apps_workspace,
        &cancel,
    )
    .unwrap();
    assert_eq!(
        apps.restore_after_home(
            &mut home_review,
            &NativeOfflineRunner,
            &home,
            &apps_workspace,
            &cancel
        )
        .unwrap(),
        2
    );
    verify_target(&mut apps, &mut home_review, &home, &apps_workspace, &cancel);
}
fn verify_target(
    apps: &mut ApplicationImport,
    home_review: &mut PortableHomeReview,
    home: &Path,
    apps_workspace: &Path,
    cancel: &AtomicBool,
) {
    use crate::setup::flatpak_transfer::Runner as _;
    assert_eq!(apps.manifest().deployments.len(), 2);
    for deployment in &apps.manifest().deployments {
        let actual = NativeOfflineRunner
            .run(
                home,
                &[
                    "info".into(),
                    "--user".into(),
                    "--show-commit".into(),
                    deployment.installed.reference.clone(),
                ],
            )
            .unwrap();
        assert_eq!(
            actual.trim(),
            deployment.commit,
            "actual deployed commit must match signed source closure"
        );
    }
    for reference in ["app/org.tilo.Fixture", "runtime/org.tilo.FixtureRuntime"] {
        assert_eq!(
            fs::read(home.join(format!(
                ".local/share/flatpak/{reference}/x86_64/stable/active/files/bin/fixture"
            )))
            .unwrap(),
            b"#!/bin/sh\nexit 0\n"
        );
    }
    assert_eq!(
        fs::read(home.join(".var/app/so.test/data/document")).unwrap(),
        b"hidden full-home app bytes"
    );
    assert_eq!(
        fs::read(home.join("owner-document")).unwrap(),
        b"unchanged original owner bytes"
    );
    assert_eq!(
        fs::read(home.join("group-only")).unwrap(),
        b"actual supplementary-group source bytes"
    );
    if mounted() {
        super::mounted::verify_import(home);
    }
    let hidden = fs::metadata(home.join(".var/app/so.test/data/document")).unwrap();
    let linked = fs::metadata(home.join("same-document")).unwrap();
    assert_eq!((hidden.dev(), hidden.ino()), (linked.dev(), linked.ino()));
    // Retry invokes actual independent baseline/installed-ref proof; a saved
    // completion receipt is not authority for this assertion.
    let retry = crate::setup::flatpak_transfer::portable::native_tests::ObservedNative::default();
    assert_eq!(
        apps.restore_after_home(home_review, &retry, home, apps_workspace, cancel)
            .unwrap(),
        2
    );
    assert_eq!(
        retry.install_count(),
        0,
        "native retry must verify without reinstalling refs"
    );
    assert_eq!(
        fs::read(home.join(".var/app/so.test/data/document")).unwrap(),
        b"hidden full-home app bytes"
    );
}

pub(super) fn verify_guided_home(home: &Path) {
    assert_eq!(
        fs::read(home.join("Documents/notes")).unwrap(),
        b"all my actual documents"
    );
    assert_eq!(
        fs::read(home.join(".var/app/story.app/state")).unwrap(),
        b"original hidden app state"
    );
    assert!(
        !home
            .join(".local/state/tilo/installer/preparations")
            .exists()
    );
}
