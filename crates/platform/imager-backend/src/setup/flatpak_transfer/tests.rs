use super::*;
use std::{cell::RefCell, collections::VecDeque};

struct Fake {
    outputs: RefCell<VecDeque<Result<String, String>>>,
    calls: RefCell<Vec<(PathBuf, Vec<String>)>>,
}
impl Fake {
    fn new(outputs: Vec<Result<&str, &str>>) -> Self {
        Self {
            outputs: RefCell::new(
                outputs
                    .into_iter()
                    .map(|output| output.map(str::to_owned).map_err(str::to_owned))
                    .collect(),
            ),
            calls: RefCell::default(),
        }
    }
}
impl Runner for Fake {
    fn run(&self, home: &Path, arguments: &[String]) -> Result<String, String> {
        self.calls
            .borrow_mut()
            .push((home.into(), arguments.to_vec()));
        self.outputs
            .borrow_mut()
            .pop_front()
            .expect("unexpected command")
    }
}

#[test]
fn inventory_and_restoration_use_named_installations_and_target_user() {
    let runner = Fake::new(vec![
        Ok(
            "org.example.Chat/x86_64/stable\tflathub\tuser\tChat\norg.example.Paint/x86_64/stable\tcompany\tsystem (work)\tPaint\n",
        ),
        Ok("org.example.Platform/x86_64/24.08\tflathub\tsystem\tPlatform\n"),
        Ok("flathub\thttps://dl.flathub.org/repo/\t\n"),
        Ok("flathub\thttps://dl.flathub.org/repo/\t\n"),
        Ok("company\thttps://example.org/repo/\t\n"),
    ]);
    let inventory = discover(&runner, Path::new("/source/home")).unwrap();
    assert_eq!(inventory.refs.len(), 3);
    assert_eq!(inventory.refs[0].app_id(), Some("org.example.Chat"));
    assert_eq!(inventory.remotes.len(), 3);
    assert!(
        runner
            .calls
            .borrow()
            .iter()
            .any(|(_, arguments)| arguments.contains(&"--installation=work".into()))
    );
    let runner = Fake::new(vec![Ok(""), Ok(""), Ok(""), Err("offline")]);
    let approved = vec![
        ApprovedRemote {
            source_installation: Installation::System("default".into()),
            source_name: "flathub".into(),
            target_name: "migrated-flathub".into(),
            definition: "/approved/flathub.flatpakrepo".into(),
        },
        ApprovedRemote {
            source_installation: Installation::System("work".into()),
            source_name: "company".into(),
            target_name: "migrated-work".into(),
            definition: "/approved/company.flatpakrepo".into(),
        },
    ];
    let selected = inventory
        .refs
        .iter()
        .filter(|item| item.installation != Installation::User)
        .cloned()
        .collect::<Vec<_>>();
    let report = restore_system_refs(
        &runner,
        Path::new("/target/home"),
        &selected,
        &approved,
        Some(Path::new("/export/.ostree/repo")),
    )
    .unwrap();
    assert_eq!(
        report.restored,
        ["runtime/org.example.Platform/x86_64/24.08"]
    );
    assert_eq!(
        report.failed,
        [(
            "app/org.example.Paint/x86_64/stable".into(),
            "offline".into()
        )]
    );
    let calls = runner.calls.borrow();
    assert!(
        calls.iter().all(
            |(home, args)| home == Path::new("/target/home") && args.contains(&"--user".into())
        )
    );
    assert!(
        calls[1]
            .1
            .contains(&"--sideload-repo=/export/.ostree/repo".into())
    );
    assert!(calls.iter().all(|(_, args)| {
        !args
            .iter()
            .any(|arg| arg.contains("no-gpg-verify") || arg == "run")
    }));
}

#[test]
fn malformed_inventory_and_unapproved_or_ambiguous_restores_do_not_run() {
    for line in [
        "app/../../evil\tflathub\tuser\tBad",
        "app/org.example.App/x86_64/stable\t--evil\tuser\tBad",
        "truncated",
    ] {
        let runner = Fake::new(vec![Ok(line)]);
        assert!(discover(&runner, Path::new("/source")).is_err());
        assert_eq!(runner.calls.borrow().len(), 1);
    }
    let item = InstalledRef {
        reference: "app/org.example.App/x86_64/stable".into(),
        origin: "flathub".into(),
        installation: Installation::System("default".into()),
        name: "App".into(),
    };
    let runner = Fake::new(vec![]);
    let report =
        restore_system_refs(&runner, Path::new("/target"), &[item.clone()], &[], None).unwrap();
    assert_eq!(report.failed.len(), 1);
    assert!(runner.calls.borrow().is_empty());
    let other = InstalledRef {
        installation: Installation::System("work".into()),
        ..item.clone()
    };
    assert!(
        restore_system_refs(
            &runner,
            Path::new("/target"),
            &[item.clone(), other],
            &[],
            None
        )
        .is_err()
    );
    assert!(runner.calls.borrow().is_empty());
    let remote = ApprovedRemote {
        source_installation: item.installation.clone(),
        source_name: "flathub".into(),
        target_name: "flathub".into(),
        definition: "/approved/flathub.flatpakrepo".into(),
    };
    assert!(
        restore_system_refs(
            &runner,
            Path::new("/target"),
            &[item],
            &[remote.clone(), remote],
            None
        )
        .is_err()
    );
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn sideload_export_passes_literal_refs_without_shell_or_source_writes() {
    let runner = Fake::new(vec![Ok("")]);
    export_sideload(
        &runner,
        Path::new("/source"),
        &Installation::System("work".into()),
        &["app/org.example.App/x86_64/stable".into()],
        Path::new("/private/export"),
    )
    .unwrap();
    assert_eq!(
        runner.calls.borrow()[0].1,
        [
            "create-usb",
            "--installation=work",
            "/private/export",
            "app/org.example.App/x86_64/stable"
        ]
    );
    assert!(
        export_sideload(
            &runner,
            Path::new("/source"),
            &Installation::User,
            &["--delete".into()],
            Path::new("/private/export")
        )
        .is_err()
    );
    assert_eq!(runner.calls.borrow().len(), 1);
}

#[test]
fn repository_extraction_preserves_existing_key_and_refuses_links_or_unsigned_sources() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("repo")).unwrap();
    let key_path = root.join("repo/flathub.trustedkeys.gpg");
    std::fs::write(&key_path, [0x99, 0x01, 0xff]).unwrap();
    let mut remote = Remote {
        installation: Installation::System("default".into()),
        name: "flathub".into(),
        url: "https://dl.flathub.org/repo/".into(),
        options: String::new(),
    };
    let definition = String::from_utf8(extract_remote_definition(&remote, &root).unwrap()).unwrap();
    assert!(definition.contains(&format!("GPGKey={}\n", STANDARD.encode([0x99, 0x01, 0xff]))));
    assert_eq!(std::fs::read(&key_path).unwrap(), [0x99, 0x01, 0xff]);
    remote.options = "no-gpg-verify".into();
    assert!(extract_remote_definition(&remote, &root).is_err());
    remote.options.clear();
    remote.url = "https://person:secret@example.org/repo/".into();
    assert!(extract_remote_definition(&remote, &root).is_err());
    #[cfg(unix)]
    {
        remote.url = "https://dl.flathub.org/repo/".into();
        std::fs::remove_file(&key_path).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", &key_path).unwrap();
        assert!(extract_remote_definition(&remote, &root).is_err());
    }
}

#[test]
fn standalone_data_selection_preserves_all_app_data_without_duplicate_home_jobs() {
    let source = Path::new("/source/home");
    let target = Path::new("/target/home");
    let journals = Path::new("/private/journals");
    let jobs = data_subtrees(source, target, journals, false).unwrap();
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0].source, source.join(".local/share/flatpak"));
    assert_eq!(jobs[1].destination, target.join(".var/app"));
    assert!(
        data_subtrees(source, target, journals, true)
            .unwrap()
            .is_empty()
    );
    assert!(data_subtrees(Path::new("../source"), target, journals, false).is_err());
}

#[test]
fn custom_installation_root_comes_from_matching_deployment_location() {
    let reference = InstalledRef {
        reference: "app/org.example.App/x86_64/stable".into(),
        origin: "flathub".into(),
        installation: Installation::System("work".into()),
        name: "App".into(),
    };
    let inventory = Inventory {
        refs: vec![reference],
        remotes: Vec::new(),
    };
    let location = format!(
        "/mnt/apps/work/app/org.example.App/x86_64/stable/{}\n",
        "a".repeat(64)
    );
    let runner = Fake::new(vec![Ok(&location)]);
    assert_eq!(
        installation_roots(&runner, Path::new("/source"), &inventory)
            .unwrap()
            .get(&Installation::System("work".into())),
        Some(&PathBuf::from("/mnt/apps/work"))
    );
    let runner = Fake::new(vec![Ok("/mnt/apps/../../other\n")]);
    assert!(installation_roots(&runner, Path::new("/source"), &inventory).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn standalone_app_data_uses_verified_home_worker_and_keeps_source_and_existing_target() {
    use crate::setup::migration_worker::{WorkerEvent, WorkerOutcome};
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    let target = root.join("target");
    let journals = root.join("journals");
    for directory in [&source, &target, &journals] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::create_dir_all(source.join(".var/app/org.example.Chat/data")).unwrap();
    std::fs::create_dir_all(target.join(".var")).unwrap();
    std::fs::write(
        source.join(".var/app/org.example.Chat/data/session"),
        b"opaque-account-session",
    )
    .unwrap();
    std::fs::write(
        source.join(".var/app/org.example.Chat/data/helper"),
        b"example-data-executable",
    )
    .unwrap();
    std::fs::set_permissions(
        source.join(".var/app/org.example.Chat/data/helper"),
        std::fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "session",
        source.join(".var/app/org.example.Chat/data/latest"),
    )
    .unwrap();
    let jobs = data_subtrees(&source, &target, &journals, false).unwrap();
    let review = review_subtree(&jobs[1]).unwrap();
    let mut worker = review.start().unwrap();
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "Flatpak data worker did not finish"
        );
        match worker.try_next() {
            Some(WorkerEvent::Finished(WorkerOutcome::Complete(report))) => {
                assert!(report.complete());
                break;
            }
            Some(WorkerEvent::Finished(_)) => panic!("Flatpak data transfer did not complete"),
            _ => std::thread::yield_now(),
        }
    }
    let copied = target.join(".var/app/org.example.Chat/data");
    assert_eq!(
        std::fs::read(copied.join("session")).unwrap(),
        b"opaque-account-session"
    );
    assert_eq!(
        std::fs::read_link(copied.join("latest")).unwrap(),
        Path::new("session")
    );
    assert_eq!(
        std::fs::metadata(copied.join("helper"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert_eq!(
        std::fs::read(source.join(".var/app/org.example.Chat/data/session")).unwrap(),
        b"opaque-account-session"
    );
    assert!(review_subtree(&jobs[1]).is_err());
    assert_eq!(
        std::fs::read(copied.join("session")).unwrap(),
        b"opaque-account-session"
    );
}

#[test]
fn native_named_installation_ids_do_not_use_display_names() {
    assert_eq!(
        installation("system (native-fixture)").unwrap(),
        Installation::System("native-fixture".into())
    );
    for malformed in [
        "system (Native fixture)",
        "system ()",
        "system (fixture))",
        "system (../fixture)",
        "system (user)",
        "system (default)",
    ] {
        assert!(installation(malformed).is_err(), "must refuse {malformed}");
    }
}
