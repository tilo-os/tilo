use super::*;
use std::{fs, os::unix::fs::PermissionsExt};
struct ChangedLatest {
    calls: std::cell::RefCell<Vec<Vec<String>>>,
    installed: std::cell::RefCell<String>,
    substitute: bool,
}
impl Runner for ChangedLatest {
    fn run(&self, _home: &Path, arguments: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(arguments.to_vec());
        match arguments.first().map(String::as_str) {
            Some("remote-add") => Ok(String::new()),
            Some("install") => {
                let requested = arguments
                    .iter()
                    .find_map(|arg| arg.strip_prefix("--commit="));
                let actual = if self.substitute { None } else { requested };
                *self.installed.borrow_mut() = actual.map_or_else(|| "b".repeat(64), str::to_owned);
                Ok(String::new())
            }
            Some("info") if arguments.iter().any(|arg| arg == "--show-origin") => {
                Ok("reviewed-origin".into())
            }
            Some("info") if arguments.iter().any(|arg| arg == "--show-commit") => {
                Ok(self.installed.borrow().clone())
            }
            _ => Err("Unexpected target operation".into()),
        }
    }
}
#[test]
fn source_scoped_restore_pins_reviewed_commit_when_remote_latest_changes() {
    let (_temp, cli, selected) = fixture();
    let retained = HandoffInventory::capture(
        &cli,
        Path::new("/source-home"),
        vec![selected.clone()],
        |_| Ok(metadata_plan()),
    )
    .unwrap();
    let approved = ApprovedRemote {
        source_installation: selected.installation.clone(),
        source_name: selected.origin.clone(),
        target_name: "reviewed-origin".into(),
        definition: cli.root.join("approved.flatpakrepo"),
    };
    for substitute in [false, true] {
        // The retained metadata is minted by actual capture above. This fixture
        // exercises the canonical target adapter/restore transaction; the real
        // readonly mount qualification is deliberately a separate requirement.
        let runner = SourceScopedRunner {
            source_home: "/source-home".into(),
            inventory: retained.inventory.clone(),
            roots: BTreeMap::new(),
            deployments: BTreeMap::new(),
            selected_commits: retained
                .installations
                .iter()
                .flat_map(|source| source.deployments.iter())
                .map(|deployment| (deployment.reference.clone(), deployment.commit.clone()))
                .collect(),
            leases: Vec::new(),
            target: ChangedLatest {
                calls: std::cell::RefCell::new(Vec::new()),
                installed: std::cell::RefCell::new(String::new()),
                substitute,
            },
        };
        let report = super::super::restore_system_refs(
            &runner,
            Path::new("/target-home"),
            retained.selected(),
            std::slice::from_ref(&approved),
            None,
        )
        .unwrap();
        let calls = runner.target.calls.borrow();
        let install = calls
            .iter()
            .find(|args| args.first().is_some_and(|verb| verb == "install"))
            .unwrap();
        assert!(install.contains(&format!("--commit={}", "a".repeat(64))));
        assert!(install.contains(&"--no-deps".into()));
        let installs: Vec<_> = calls
            .iter()
            .filter(|args| args.first().is_some_and(|verb| verb == "install"))
            .collect();
        assert_eq!(installs.len(), 2);
        assert_eq!(
            installs.first().unwrap().last().unwrap(),
            "runtime/org.example.Platform/x86_64/stable"
        );
        for command in installs {
            assert!(command.contains(&format!("--commit={}", "a".repeat(64))));
            assert!(command.contains(&"--no-deps".into()));
            assert!(command.contains(&"--no-related".into()));
        }
        if substitute {
            assert!(report.restored.is_empty());
            assert_eq!(report.failed.len(), 2);
            for (_, message) in &report.failed {
                assert_eq!(
                    message,
                    "Installed Flatpak commit differs from the reviewed deployment"
                );
            }
        } else {
            assert_eq!(
                report.restored,
                vec![
                    "runtime/org.example.Platform/x86_64/stable".to_owned(),
                    selected.reference.clone()
                ]
            );
            assert!(report.failed.is_empty());
            assert_eq!(*runner.target.installed.borrow(), "a".repeat(64));
        }
        assert!(
            runner
                .run(
                    Path::new("/target-home"),
                    &[
                        "install".into(),
                        "--commit=unreviewed".into(),
                        "reviewed-origin".into(),
                        selected.reference.clone(),
                    ]
                )
                .is_err()
        );
    }
}
struct InventoryCli {
    root: PathBuf,
    calls: std::cell::RefCell<Vec<Vec<String>>>,
}
impl Runner for InventoryCli {
    fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
        self.calls.borrow_mut().push(args.to_vec());
        match args.first().map(String::as_str) {
            Some("list") if args[2] == "--app" => {
                Ok("org.example.Chat/x86_64/stable\tflathub\twork\tChat\n".into())
            }
            Some("list" | "remotes") if args[1] == "--user" => Ok(String::new()),
            Some("list") => {
                Ok("org.example.Platform/x86_64/stable\tflathub\twork\tPlatform\n".into())
            }
            Some("remotes") => Ok("flathub\thttps://example.org/repo/\t\n".into()),
            Some("info") if args.contains(&"--show-runtime".into()) => {
                Ok("org.example.Platform/x86_64/stable".into())
            }
            Some("info") if args.contains(&"--show-extensions".into()) => {
                Ok(args.last().unwrap().clone())
            }
            Some("info") => Ok(self
                .root
                .join(args.last().unwrap())
                .join("a".repeat(64))
                .to_string_lossy()
                .into_owned()),
            _ => Err("Unexpected native query".into()),
        }
    }
}
fn metadata_plan() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"schema":1,"operation_id":"flatpak-fixture","categories":{"home":false,"flatpak":true,"accounts":false},"home":{"volume":{"disk_guid":"11111111-1111-4111-8111-111111111111","disk_bytes":(128u64 << 30),"sector_bytes":512,"partition_guid":"22222222-2222-4222-8222-222222222222","first_lba":2048,"last_lba":1048575,"filesystem_uuid":"33333333-3333-4333-8333-333333333333","filesystem":"ext4"},"relative":{"platform":"Unix","units":b"var/lib/flatpak"},"inode":1234,"original_uid":0,"subvolume":null}})).unwrap()
}
#[test]
fn attested_inventory_rebinds_stage_operation_and_refuses_changes_or_overlap() {
    use crate::setup::{migration_handoff, staging};
    let (_temp, cli, selected) = fixture();
    let retained = HandoffInventory::capture(
        &cli,
        Path::new("/source-home"),
        vec![selected.clone()],
        |_| Ok(metadata_plan()),
    )
    .unwrap();
    let sample = migration_handoff::Plan::parse(&metadata_plan()).unwrap();
    let source = &sample.home.volume;
    let mut basis = staging::plan::tests::linux_basis(
        512,
        source.disk_bytes,
        vec![staging::platform::Partition {
            number: 1,
            guid: source.partition_guid.clone(),
            type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
            first_lba: source.first_lba,
            last_lba: source.last_lba,
        }],
    );
    basis.disk.guid.clone_from(&source.disk_guid);
    let installation = staging::plan::compute(
        &basis,
        staging::plan::Requirements {
            staging_esp_bytes: 600 << 20,
            staging_root_bytes: 40 << 30,
            target_bytes: staging::MIN_TARGET_BYTES,
        },
    )
    .unwrap();
    let fresh = || {
        HandoffInventory::capture(
            &cli,
            Path::new("/source-home"),
            vec![selected.clone()],
            |_| {
                let mut plan = sample.clone();
                plan.categories = migration_handoff::Categories {
                    home: false,
                    flatpak: true,
                    accounts: false,
                };
                Ok(plan.bytes().unwrap())
            },
        )
        .unwrap()
    };
    let operation = uuid::Uuid::new_v4().to_string();
    let rebound = retained
        .rebind_attested(fresh(), &sample.operation_id, &operation, &installation)
        .unwrap();
    rebound.validate_operation(&operation).unwrap();
    for (_, bytes) in rebound.sources() {
        assert_eq!(
            migration_handoff::Plan::parse(bytes).unwrap().operation_id,
            operation
        );
    }
    let mut overlap = installation.clone();
    overlap.target_extent.first_lba = source.first_lba;
    assert!(
        retained
            .rebind_attested(fresh(), &sample.operation_id, &operation, &overlap)
            .is_err()
    );
    fs::write(
        cli.root.join("repo/flathub.trustedkeys.gpg"),
        b"changed original signing trust",
    )
    .unwrap();
    assert!(
        retained
            .rebind_attested(fresh(), &sample.operation_id, &operation, &installation)
            .is_err()
    );
}
fn fixture() -> (tempfile::TempDir, InventoryCli, InstalledRef) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.join("repo")).unwrap();
    fs::write(root.join("repo/config"), b"[core]\nmode=bare-user-only\n[remote \"flathub\"]\nurl=https://example.org/repo/\ngpg-verify=true\n").unwrap();
    fs::write(
        root.join("repo/flathub.trustedkeys.gpg"),
        b"original public signing key",
    )
    .unwrap();
    let parent = root.join("app/org.example.Chat/x86_64/stable");
    let deployment = parent.join("a".repeat(64));
    fs::create_dir_all(&deployment).unwrap();
    fs::write(deployment.join("deploy"), b"original deployment metadata").unwrap();
    std::os::unix::fs::symlink("a".repeat(64), parent.join("active")).unwrap();
    let runtime_parent = root.join("runtime/org.example.Platform/x86_64/stable");
    fs::create_dir_all(runtime_parent.join("a".repeat(64))).unwrap();
    fs::write(
        runtime_parent.join("a".repeat(64)).join("deploy"),
        b"runtime metadata",
    )
    .unwrap();
    std::os::unix::fs::symlink("a".repeat(64), runtime_parent.join("active")).unwrap();
    let selected = InstalledRef {
        reference: "app/org.example.Chat/x86_64/stable".into(),
        origin: "flathub".into(),
        installation: Installation::System("work".into()),
        name: "Chat".into(),
    };
    (
        temp,
        InventoryCli {
            root,
            calls: std::cell::RefCell::default(),
        },
        selected,
    )
}
#[test]
fn persisted_flatpak_inventory_is_bounded_metadata_and_revalidates_original_trust() {
    let (_temp, cli, selected) = fixture();
    let mut bound = Vec::new();
    let handoff = HandoffInventory::capture(
        &cli,
        Path::new("/source-home"),
        vec![selected.clone()],
        |root| {
            bound.push(root.to_owned());
            Ok(metadata_plan())
        },
    )
    .unwrap();
    assert_eq!(bound, [cli.root.clone()]);
    assert_eq!(handoff.selected().len(), 2);
    assert!(handoff.selected().contains(&selected));
    assert!(
        handoff
            .selected()
            .iter()
            .any(|item| item.reference == "runtime/org.example.Platform/x86_64/stable")
    );
    assert_eq!(handoff.sources().count(), 1);
    let bytes = handoff.bytes().unwrap();
    assert!(
        !bytes
            .windows(b"original public signing key".len())
            .any(|part| part == b"original public signing key")
    );
    assert!(
        !bytes
            .windows(b"original deployment metadata".len())
            .any(|part| part == b"original deployment metadata")
    );
    let parsed = HandoffInventory::parse(&bytes).unwrap();
    assert_eq!(parsed, handoff);
    parsed.installations[0].verify_original(&cli.root).unwrap();
    fs::write(
        cli.root.join("repo/flathub.trustedkeys.gpg"),
        b"replacement trust must fail",
    )
    .unwrap();
    assert!(parsed.installations[0].verify_original(&cli.root).is_err());
}
#[test]
fn original_deployment_change_and_writable_source_grants_are_refused() {
    let (_temp, cli, selected) = fixture();
    let handoff =
        HandoffInventory::capture(&cli, Path::new("/source-home"), vec![selected], |_| {
            Ok(metadata_plan())
        })
        .unwrap();
    fs::write(
        cli.root
            .join("app/org.example.Chat/x86_64/stable")
            .join("a".repeat(64))
            .join("deploy"),
        b"different metadata",
    )
    .unwrap();
    assert!(handoff.installations[0].verify_original(&cli.root).is_err());
    let pin = PinnedDirectory::open_absolute(&cli.root).unwrap();
    assert!(ReadOnlySource::from_resolved(&cli.root, pin, Arc::new(())).is_err());
}
#[test]
fn handoff_refuses_unbound_ambiguous_and_secret_bearing_metadata() {
    let (_temp, cli, selected) = fixture();
    let handoff = HandoffInventory::capture(
        &cli,
        Path::new("/source-home"),
        vec![selected.clone()],
        |_| Ok(metadata_plan()),
    )
    .unwrap();
    assert!(
        HandoffInventory::capture(
            &cli,
            Path::new("/source-home"),
            vec![selected.clone(), selected],
            |_| Ok(metadata_plan())
        )
        .is_err()
    );
    for malformed in [
        {
            let mut value = serde_json::to_value(&handoff).unwrap();
            value["installations"] = serde_json::json!([]);
            value
        },
        {
            let mut value = serde_json::to_value(&handoff).unwrap();
            value["inventory"]["remotes"][0]["url"] =
                "https://example.org/repo/?access_token=secret".into();
            value
        },
        {
            let mut value = serde_json::to_value(&handoff).unwrap();
            value["credentials"] = "secret".into();
            value
        },
    ] {
        assert!(HandoffInventory::parse(&serde_json::to_vec(&malformed).unwrap()).is_err());
    }
}

#[derive(Default)]
struct RoutingTarget(std::cell::RefCell<Vec<(PathBuf, Vec<String>)>>);
impl Runner for RoutingTarget {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        self.0.borrow_mut().push((home.to_owned(), args.to_vec()));
        if args.contains(&"--show-commit".into()) {
            return Ok("a".repeat(64));
        }
        if args.contains(&"--show-origin".into()) {
            return Ok("reviewed-origin".into());
        }
        Ok("target native command result".into())
    }
}

#[test]
fn source_scoped_adapter_routes_source_queries_and_target_execution_separately() {
    struct EmptySource;
    impl Runner for EmptySource {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            assert!(matches!(
                args.first().map(String::as_str),
                Some("list" | "remotes")
            ));
            if args.first().is_some_and(|verb| verb == "remotes") && args[1] == "--user" {
                return Ok("flathub\thttps://example.org/repo/\t\n".into());
            }
            Ok(String::new())
        }
    }
    let home = Path::new("/explicit-original-home");
    let mut metadata = HandoffInventory::capture(&EmptySource, home, Vec::new(), |_| {
        panic!("Empty inventory must not request source grants")
    })
    .unwrap();
    // Capture excludes unselected remotes. Exercise the adapter's retained
    // public-metadata protocol through the validated persisted format, without
    // inventing any physical source grant or installed application.
    assert!(metadata.inventory.remotes.is_empty());
    metadata.inventory.remotes = discover(&EmptySource, home).unwrap().remotes;
    let metadata = HandoffInventory::parse(&metadata.bytes().unwrap()).unwrap();
    let target = RoutingTarget::default();
    let adapter = metadata
        .resolve(home, target, |_| {
            panic!("Empty inventory must not invent readonly grants")
        })
        .unwrap();
    let source_inventory = discover(&adapter, home).unwrap();
    assert!(source_inventory.refs.is_empty());
    assert_eq!(
        source_inventory.remotes,
        [Remote {
            installation: Installation::User,
            name: "flathub".into(),
            url: "https://example.org/repo/".into(),
            options: String::new(),
        }]
    );
    assert!(adapter.target.0.borrow().is_empty());
    let args = vec!["install".into(), "--user".into(), "org.example.Chat".into()];
    assert!(adapter.run(home, &args).is_err());
    assert!(adapter.target.0.borrow().is_empty());
    assert!(adapter.run(Path::new("/fresh-target-home"), &args).is_err());
    assert!(adapter.target.0.borrow().is_empty());

    let query = vec![
        "info".into(),
        "--user".into(),
        "--show-origin".into(),
        "app/org.example.Chat/x86_64/stable".into(),
    ];
    assert_eq!(
        adapter
            .run(Path::new("/fresh-target-home"), &query)
            .unwrap(),
        "reviewed-origin"
    );
    assert_eq!(
        adapter.target.0.borrow().as_slice(),
        [(PathBuf::from("/fresh-target-home"), query)]
    );
}

#[test]
fn source_scoped_target_routes_only_retained_pinned_installs() {
    let home = Path::new("/explicit-original-home");
    let (_temp, cli, selected) = fixture();
    let retained =
        HandoffInventory::capture(&cli, home, vec![selected.clone()], |_| Ok(metadata_plan()))
            .unwrap();
    // Real captured metadata authorizes this target invocation. Readonly mount
    // attestation is qualified separately; this fixture never fabricates a lease.
    let adapter = SourceScopedRunner {
        source_home: home.to_owned(),
        inventory: retained.inventory.clone(),
        roots: BTreeMap::new(),
        deployments: BTreeMap::new(),
        selected_commits: retained
            .installations
            .iter()
            .flat_map(|source| source.deployments.iter())
            .map(|deployment| (deployment.reference.clone(), deployment.commit.clone()))
            .collect(),
        leases: Vec::new(),
        target: RoutingTarget::default(),
    };
    let args = vec![
        "install".into(),
        "--user".into(),
        "reviewed-origin".into(),
        selected.reference.clone(),
    ];
    assert!(adapter.run(home, &args).is_err());
    assert!(adapter.target.0.borrow().is_empty());
    let target = Path::new("/fresh-target-home");
    assert_eq!(
        adapter.run(target, &args).unwrap(),
        "target native command result"
    );
    let calls = adapter.target.0.borrow();
    assert_eq!(calls.len(), 3);
    assert!(calls.iter().all(|(path, _)| path == target));
    let command = &calls.first().unwrap().1;
    assert_eq!(command.first().unwrap(), "install");
    assert_eq!(command.last().unwrap(), &selected.reference);
    assert!(command.contains(&format!("--commit={}", "a".repeat(64))));
    assert!(command.contains(&"--no-deps".into()));
    assert!(command.contains(&"--no-related".into()));
    assert!(calls.get(1).unwrap().1.contains(&"--show-commit".into()));
    assert!(calls.get(2).unwrap().1.contains(&"--show-origin".into()));
}

#[test]
fn applications_copy_consent_is_narrow_and_preserves_original_retained_plan() {
    use crate::setup::migration_handoff::{HomePolicy, Plan};
    let (_temp, cli, selected) = fixture();
    let inventory =
        HandoffInventory::capture(&cli, Path::new("/source-home"), vec![selected], |_| {
            Ok(metadata_plan())
        })
        .unwrap();
    let mut original = Plan::parse(&metadata_plan()).unwrap();
    original.categories.home = true;
    original.categories.accounts = true;
    original.home_policy = HomePolicy::AccessReadWrite;
    original.flatpak_inventory = Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap());
    let retained = original.clone();
    let apps = original.applications_copy_plan().unwrap();
    assert_eq!(original, retained);
    assert_eq!(apps.home, original.home);
    assert_eq!(apps.operation_id, original.operation_id);
    assert_eq!(apps.flatpak_inventory, original.flatpak_inventory);
    assert!(!apps.categories.home);
    assert!(!apps.categories.accounts);
    assert!(apps.categories.flatpak);
    assert_eq!(apps.home_policy, HomePolicy::Copy);
    original.categories.flatpak = false;
    assert!(original.applications_copy_plan().is_err());
    original.categories.flatpak = true;
    original.flatpak_inventory = None;
    assert!(original.applications_copy_plan().is_err());
}

struct SelectedUserCli(InventoryCli);
impl Runner for SelectedUserCli {
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        if args.first().is_some_and(|verb| verb == "list") {
            return if args.contains(&"--app".into()) {
                Ok("org.example.Chat/x86_64/stable\tflathub\tuser\tChat\norg.example.Excluded/x86_64/stable\tflathub\tuser\tExcluded\n".into())
            } else {
                Ok("org.example.Platform/x86_64/stable\tflathub\tuser\tPlatform\n".into())
            };
        }
        if args.first().is_some_and(|verb| verb == "remotes") {
            return Ok("flathub\thttps://example.org/repo/\t\n".into());
        }
        self.0.run(home, args)
    }
}
#[test]
fn selected_applications_intent_excludes_unselected_user_deployments_without_changing_home_intent()
{
    let (_temp, cli, mut selected) = fixture();
    let excluded_parent = cli.root.join("app/org.example.Excluded/x86_64/stable");
    fs::create_dir_all(excluded_parent.join("a".repeat(64))).unwrap();
    fs::write(
        excluded_parent.join("a".repeat(64)).join("deploy"),
        b"excluded metadata",
    )
    .unwrap();
    std::os::unix::fs::symlink("a".repeat(64), excluded_parent.join("active")).unwrap();
    selected.installation = Installation::User;
    let runner = SelectedUserCli(cli);
    let home = Path::new("/explicit-reviewed-home");
    let subset =
        HandoffInventory::capture_selected_state(&runner, home, vec![selected.clone()], |_| {
            Ok(metadata_plan())
        })
        .unwrap();
    assert!(
        HandoffInventory::capture(&runner, home, vec![selected.clone()], |_| {
            Ok(metadata_plan())
        })
        .is_err(),
        "Full Home must preserve its copied-user conflict policy"
    );
    let whole =
        HandoffInventory::capture(&runner, home, Vec::new(), |_| Ok(metadata_plan())).unwrap();
    assert!(
        subset
            .inventory
            .refs
            .iter()
            .all(|item| item.reference != "app/org.example.Excluded/x86_64/stable")
    );
    assert!(
        whole
            .inventory
            .refs
            .iter()
            .any(|item| item.reference == "app/org.example.Excluded/x86_64/stable")
    );
    assert!(whole.selected().is_empty());
    assert!(subset.selected().contains(&selected));
    let reopened = HandoffInventory::parse(&subset.bytes().unwrap()).unwrap();
    assert_eq!(reopened, subset);
    assert!(reopened.selected().contains(&selected));
    assert!(
        reopened
            .selected()
            .iter()
            .any(|item| item.reference == "runtime/org.example.Platform/x86_64/stable")
    );
    assert_eq!(
        fs::read(excluded_parent.join("a".repeat(64)).join("deploy")).unwrap(),
        b"excluded metadata"
    );
}
