use super::*;
use std::{cell::RefCell, collections::VecDeque, os::unix::fs::PermissionsExt, path::Path};

struct FixtureRunner {
    replies: RefCell<VecDeque<Result<String, String>>>,
    calls: RefCell<Vec<Vec<String>>>,
    target: PathBuf,
}
impl Runner for FixtureRunner {
    fn verify_repository(&self, home: &Path) -> Result<(), String> {
        assert_eq!(home, self.target);
        let actual = std::fs::read(home.join(".local/share/flatpak/repo/fixture-object"))
            .map_err(|error| error.to_string())?;
        if actual != b"installed immutable object" {
            return Err("Fixture object integrity failed".into());
        }
        Ok(())
    }
    fn run(&self, home: &Path, args: &[String]) -> Result<String, String> {
        if args
            .first()
            .is_some_and(|verb| matches!(verb.as_str(), "remote-add" | "install"))
        {
            assert_eq!(home, self.target);
            assert_eq!(
                std::fs::read(home.join(".var/app/org.example.App/data/session")).unwrap(),
                b"session"
            );
        }
        self.calls.borrow_mut().push(args.to_vec());
        let state = home.join(".local/share/flatpak/.fixture-installed");
        if home == self.target && args.first().map(String::as_str) == Some("list") {
            if !state.exists() || args.iter().any(|arg| arg == "--runtime") {
                return Ok(String::new());
            }
            let value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(state).unwrap()).unwrap();
            return Ok(format!(
                "{}\t{}\n",
                value["reference"]
                    .as_str()
                    .unwrap()
                    .trim_start_matches("app/"),
                value["origin"].as_str().unwrap()
            ));
        }
        if home == self.target && args.iter().any(|arg| arg == "--show-commit") {
            assert!(state.exists());
            return Ok("a".repeat(64));
        }
        let result = self
            .replies
            .borrow_mut()
            .pop_front()
            .expect("Unexpected Flatpak command");
        if home == self.target
            && result.is_ok()
            && args.first().map(String::as_str) == Some("remote-add")
        {
            std::fs::create_dir_all(home.join(".local/share/flatpak/repo")).unwrap();
            std::fs::write(
                home.join(".local/share/flatpak/repo/config"),
                b"[core]\nmode=bare-user-only\n",
            )
            .unwrap();
        }
        if home == self.target
            && result.is_ok()
            && args.first().map(String::as_str) == Some("install")
        {
            let reference = args.last().unwrap();
            let origin = args.get(args.len() - 2).unwrap();
            std::fs::write(
                home.join(".local/share/flatpak/repo/fixture-object"),
                b"installed immutable object",
            )
            .unwrap();
            std::fs::write(
                state,
                serde_json::to_vec(&serde_json::json!({"reference":reference,"origin":origin}))
                    .unwrap(),
            )
            .unwrap();
        }
        result
    }
}
fn fixture(flatpak: bool) -> (tempfile::TempDir, TransferRequest, FixtureRunner) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let source = root.join("source");
    let journals = root.join("journals");
    for directory in [&source, &journals] {
        std::fs::create_dir(directory).unwrap();
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::create_dir_all(source.join(".var/app/org.example.App/data")).unwrap();
    std::fs::write(
        source.join(".var/app/org.example.App/data/session"),
        b"session",
    )
    .unwrap();
    std::fs::write(source.join("personal"), b"personal").unwrap();
    let system = root.join("system");
    std::fs::create_dir_all(system.join("repo")).unwrap();
    std::fs::write(
        system.join("repo/flathub.trustedkeys.gpg"),
        b"existing-key-packets",
    )
    .unwrap();
    let target = root.join("new-home");
    let replies = if flatpak {
        vec![
            Ok("org.example.App/x86_64/stable\tflathub\tsystem\tApp\n".into()),
            Ok(String::new()),
            Ok(String::new()),
            Ok("flathub\thttps://dl.flathub.org/repo/\t\n".into()),
            Ok(format!(
                "{}/app/org.example.App/x86_64/stable/{}\n",
                system.display(),
                "a".repeat(64)
            )),
            Ok(String::new()),
            Ok(String::new()),
        ]
    } else {
        vec![]
    };
    let request = TransferRequest {
        source_home: source,
        target_home: target.clone(),
        private_journals: journals,
        copy_home: true,
        copy_flatpak: flatpak,
        system_refs: if flatpak {
            vec![InstalledRef {
                reference: "app/org.example.App/x86_64/stable".into(),
                origin: "flathub".into(),
                installation: flatpak::Installation::System("default".into()),
                name: "App".into(),
            }]
        } else {
            vec![]
        },
    };
    let runner = FixtureRunner {
        replies: RefCell::new(replies.into()),
        calls: RefCell::default(),
        target,
    };
    (temp, request, runner)
}

#[test]
fn runtime_socket_is_disclosed_without_blocking_home_data() {
    let (_temp, request, runner) = fixture(false);
    let socket_path = request.source_home.join("agent.sock");
    let _socket = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let reviewed = review(&runner, request.clone()).unwrap();
    let summary = reviewed.summary().unwrap();
    assert_eq!(summary.runtime_sockets, 1);
    assert_eq!(summary.special_files, 0);
    let report = reviewed
        .execute(&runner, &AtomicBool::new(false), false, |_| {})
        .unwrap();
    assert!(report.complete(), "{report:?}");
    assert!(!request.target_home.join("agent.sock").exists());
    assert!(socket_path.exists());
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    assert!(report.copies.iter().any(|copy| {
        copy.metadata_limitations
            .iter()
            .any(|disclosure| disclosure.contains("agent.sock"))
    }));
}

#[test]
fn actual_home_data_precedes_target_flatpak_install_and_originals_stay_unchanged() {
    let (_temp, request, runner) = fixture(true);
    let reviewed = review(&runner, request.clone()).unwrap();
    let mut progress = 0;
    let report = reviewed
        .execute(&runner, &AtomicBool::new(false), false, |event| {
            if matches!(event, TransferEvent::CopyProgress(_)) {
                progress += 1;
            }
        })
        .unwrap();
    assert!(report.complete());
    assert!(progress > 0);
    assert_eq!(
        report.flatpak.restored,
        ["app/org.example.App/x86_64/stable"]
    );
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
    assert!(
        runner
            .calls
            .borrow()
            .last()
            .unwrap()
            .contains(&"--user".into())
    );
}

#[test]
fn flatpak_only_transfers_real_data_without_other_home_files() {
    let (_temp, mut request, runner) = fixture(false);
    request.copy_home = false;
    request.copy_flatpak = true;
    std::fs::create_dir_all(request.source_home.join(".local/share/flatpak/overrides")).unwrap();
    std::fs::write(
        request
            .source_home
            .join(".local/share/flatpak/overrides/global"),
        b"permissions",
    )
    .unwrap();
    let reviewed = review(&runner, request.clone()).unwrap();
    assert_eq!(reviewed.copy_count(), 2);
    let report = reviewed
        .execute(&runner, &AtomicBool::new(false), false, |_| {})
        .unwrap();
    assert!(report.complete());
    assert!(!request.target_home.join("personal").exists());
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".local/share/flatpak/overrides/global")
        )
        .unwrap(),
        b"permissions"
    );
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn changed_source_cancellation_and_remote_failures_never_claim_completion() {
    let (_temp, request, runner) = fixture(true);
    let reviewed = review(&runner, request.clone()).unwrap();
    let cancelled = reviewed
        .execute(&runner, &AtomicBool::new(true), false, |_| {})
        .unwrap();
    assert!(!cancelled.complete());
    assert!(cancelled.cancelled);
    assert!(!request.target_home.exists());
    std::fs::write(request.source_home.join("personal"), b"changed").unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .is_err()
    );
    assert!(!request.target_home.exists());
    assert_eq!(runner.calls.borrow().len(), 5);

    let (_temp, request, runner) = fixture(true);
    let reviewed = review(&runner, request.clone()).unwrap();
    *runner.replies.borrow_mut().back_mut().unwrap() = Err("repository offline".into());
    let report = reviewed
        .execute(&runner, &AtomicBool::new(false), false, |_| {})
        .unwrap();
    assert!(!report.complete());
    assert_eq!(report.flatpak.failed.len(), 1);
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    // Approved Flatpak additions after home completion do not replay byte
    // copying or lose repository additions on application retry.
    std::fs::create_dir_all(request.target_home.join(".local/share/flatpak/repo")).unwrap();
    std::fs::write(
        request
            .target_home
            .join(".local/share/flatpak/repo/new-app"),
        b"installed-object",
    )
    .unwrap();
    runner
        .replies
        .borrow_mut()
        .extend([Ok(String::new()), Ok(String::new())]);
    let retried = reviewed
        .execute(&runner, &AtomicBool::new(false), true, |_| {})
        .unwrap();
    assert!(retried.complete());
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".local/share/flatpak/repo/new-app")
        )
        .unwrap(),
        b"installed-object"
    );
}

#[test]
fn copied_user_flatpak_version_cannot_be_replaced_by_system_selection() {
    let (_temp, request, runner) = fixture(true);
    *runner.replies.borrow_mut().front_mut().unwrap() = Ok("org.example.App/x86_64/stable\tflathub\tsystem\tApp\norg.example.App/x86_64/stable\tpersonal\tuser\tApp\n".into());
    let error = match review(&runner, request.clone()) {
        Ok(_) => panic!("Conflicting system selection was accepted"),
        Err(error) => error,
    };
    assert!(error.contains("already included"));
    assert!(!request.target_home.exists());
    assert!(!runner.calls.borrow().iter().any(|args| matches!(
        args.first().map(String::as_str),
        Some("install" | "remote-add")
    )));
}

#[test]
fn restarted_home_review_revalidates_receipts_and_source_instead_of_trusting_success() {
    let (_temp, request, runner) = fixture(false);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    drop(reviewed);
    let resumed = review(&runner, request.clone()).unwrap();
    assert!(resumed.phase.lock().unwrap().complete.is_empty());
    assert!(
        resumed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .is_err()
    );
    assert!(
        resumed
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    std::fs::write(request.target_home.join("personal"), b"changed target").unwrap();
    let resumed = recover(&runner, request.clone()).unwrap();
    if let Ok(report) = resumed.execute(&runner, &AtomicBool::new(false), true, |_| {}) {
        assert!(!report.complete());
    }
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"changed target"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
    let mut wrong = request.clone();
    wrong.copy_flatpak = true;
    assert!(recover(&runner, wrong).is_err());
    std::fs::write(request.source_home.join("personal"), b"source changed").unwrap();
    assert!(recover(&runner, request).is_err());
}

#[test]
fn restart_after_flatpak_mutation_revalidates_real_output_before_promotion() {
    let (_temp, request, runner) = fixture(true);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let (_new_temp, _new_request, fresh) = fixture(true);
    let initial: Vec<_> = fresh.replies.borrow_mut().drain(..5).collect();
    runner.replies.borrow_mut().extend(initial);
    let recovered = recover(&runner, request.clone()).unwrap();
    let before = runner
        .calls
        .borrow()
        .iter()
        .filter(|args| args.first().map(String::as_str) == Some("install"))
        .count();
    assert!(
        recovered
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    assert_eq!(
        runner
            .calls
            .borrow()
            .iter()
            .filter(|args| args.first().map(String::as_str) == Some("install"))
            .count(),
        before
    );
    std::fs::write(
        request
            .target_home
            .join(".local/share/flatpak/repo/fixture-object"),
        b"tampered object",
    )
    .unwrap();
    let (_other_temp, _other_request, fresh) = fixture(true);
    let initial: Vec<_> = fresh.replies.borrow_mut().drain(..5).collect();
    runner.replies.borrow_mut().extend(initial);
    assert!(recover(&runner, request.clone()).is_err());
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
}

#[test]
fn restart_metadata_fifo_and_symlink_are_refused_without_blocking() {
    use std::os::unix::{ffi::OsStrExt as _, fs::symlink};
    let (_temp, request, runner) = fixture(false);
    let fifo = request.private_journals.join("user-transfer-intent.json");
    let name = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: terminated fixture path; creates no object outside temporary tree.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert!(review(&runner, request.clone()).is_err());
    std::fs::remove_file(&fifo).unwrap();
    symlink(request.source_home.join("personal"), &fifo).unwrap();
    assert!(review(&runner, request.clone()).is_err());
    assert!(!request.target_home.exists());
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
}

#[test]
fn interrupted_signed_app_phase_repairs_only_reviewed_refs_and_preserves_user_data() {
    let (_temp, request, runner) = fixture(true);
    *runner.replies.borrow_mut().back_mut().unwrap() = Err("interrupted transaction".into());
    let reviewed = review(&runner, request.clone()).unwrap();
    let report = reviewed
        .execute(&runner, &AtomicBool::new(false), false, |_| {})
        .unwrap();
    assert!(!report.complete());
    std::fs::write(
        request
            .target_home
            .join(".local/share/flatpak/repo/partial-object"),
        b"partial signed download",
    )
    .unwrap();
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    runner
        .replies
        .borrow_mut()
        .extend(fresh.replies.borrow_mut().drain(..));
    let recovered = recover(&runner, request.clone()).unwrap();
    assert!(
        recovered
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    assert!(
        runner
            .calls
            .borrow()
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("install")
                && args.iter().any(|arg| arg == "--reinstall"))
    );
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"personal"
    );
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
}
#[test]
fn interrupted_app_phase_refuses_changed_userdata_before_any_retry_command() {
    let (_temp, request, runner) = fixture(true);
    *runner.replies.borrow_mut().back_mut().unwrap() = Err("interrupted transaction".into());
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        !reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    std::fs::write(
        request
            .target_home
            .join(".var/app/org.example.App/data/session"),
        b"lost userdata",
    )
    .unwrap();
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    runner
        .replies
        .borrow_mut()
        .extend(fresh.replies.borrow_mut().drain(..));
    let before = runner
        .calls
        .borrow()
        .iter()
        .filter(|args| args.first().map(String::as_str) == Some("install"))
        .count();
    assert!(recover(&runner, request.clone()).is_err());
    assert_eq!(
        runner
            .calls
            .borrow()
            .iter()
            .filter(|args| args.first().map(String::as_str) == Some("install"))
            .count(),
        before
    );
    assert_eq!(
        std::fs::read(
            request
                .source_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
}

#[test]
fn headless_worker_join_releases_source_leases_only_after_terminal_event() {
    struct Guard(Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let (_temp, request, runner) = fixture(false);
    let released = Arc::new(AtomicBool::new(false));
    let mut reviewed = review(&runner, request).unwrap();
    reviewed.retain_source_guard(Arc::new(Guard(Arc::clone(&released))));
    let mut worker = reviewed.start(false).unwrap();
    drop(reviewed);
    assert!(worker.join_finished().is_err());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(TransferEvent::Finished(result)) = worker.try_next() {
            assert!(result.unwrap().complete());
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    worker.join_finished().unwrap();
    assert!(released.load(Ordering::SeqCst));
    worker.join_finished().unwrap();
}

#[test]
fn application_retry_preserves_target_user_edits_and_refuses_unverified_adoption() {
    let (_temp, request, runner) = fixture(true);
    *runner.replies.borrow_mut().back_mut().unwrap() = Err("repository offline".into());
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        !reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    std::fs::write(request.target_home.join("personal"), b"target-user-edit").unwrap();
    let before = runner.calls.borrow().len();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .is_err()
    );
    assert_eq!(runner.calls.borrow().len(), before);
    assert_eq!(
        std::fs::read(request.target_home.join("personal")).unwrap(),
        b"target-user-edit"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
    assert!(
        reviewed
            .review_promotion(
                &request.target_home.parent().unwrap().join("active-home"),
                &request.private_journals.join("adoption")
            )
            .is_err()
    );
}

#[test]
fn flatpak_only_restart_validates_output_and_exact_original_subtrees() {
    let (_temp, mut request, runner) = fixture(true);
    request.copy_home = false;
    let installation = request.source_home.join(".local/share/flatpak");
    std::fs::create_dir_all(installation.join("overrides")).unwrap();
    std::fs::write(
        installation.join("overrides/global"),
        b"original permissions",
    )
    .unwrap();
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    assert!(!request.target_home.join("personal").exists());
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    runner
        .replies
        .borrow_mut()
        .extend(fresh.replies.borrow_mut().drain(..5));
    let restarted = recover(&runner, request.clone()).unwrap();
    let calls = runner
        .calls
        .borrow()
        .iter()
        .filter(|args| args.first().map(String::as_str) == Some("install"))
        .count();
    assert!(
        restarted
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    assert_eq!(
        runner
            .calls
            .borrow()
            .iter()
            .filter(|args| args.first().map(String::as_str) == Some("install"))
            .count(),
        calls
    );
    std::fs::write(
        request
            .target_home
            .join(".var/app/org.example.App/data/session"),
        b"user edit",
    )
    .unwrap();
    let (_fresh_temp2, _fresh_request2, fresh2) = fixture(true);
    runner
        .replies
        .borrow_mut()
        .extend(fresh2.replies.borrow_mut().drain(..5));
    assert!(recover(&runner, request.clone()).is_err());
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"user edit"
    );
    assert_eq!(
        std::fs::read(installation.join("overrides/global")).unwrap(),
        b"original permissions"
    );
}

#[test]
fn flatpak_only_interrupted_apps_repair_without_whole_home_or_original_repository() {
    let (_temp, mut request, runner) = fixture(true);
    request.copy_home = false;
    *runner.replies.borrow_mut().back_mut().unwrap() = Err("interrupted signed transaction".into());
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        !reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    assert!(!request.source_home.join(".local/share/flatpak").exists());
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    runner
        .replies
        .borrow_mut()
        .extend(fresh.replies.borrow_mut().drain(..));
    let restarted = recover(&runner, request.clone()).unwrap();
    assert!(
        restarted
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    assert!(
        runner
            .calls
            .borrow()
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("install")
                && args.iter().any(|arg| arg == "--reinstall"))
    );
    assert!(!request.target_home.join("personal").exists());
    std::fs::write(request.target_home.join(".local/outsider"), b"unreviewed").unwrap();
    let before = runner.calls.borrow().len();
    assert!(recover(&runner, request.clone()).is_err());
    assert_eq!(runner.calls.borrow().len(), before);
    assert_eq!(
        std::fs::read(request.target_home.join(".local/outsider")).unwrap(),
        b"unreviewed"
    );
}

#[test]
fn flatpak_only_data_restart_replays_journals_and_refuses_changed_permissions() {
    let (_temp, mut request, runner) = fixture(false);
    request.copy_home = false;
    request.copy_flatpak = true;
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let restarted = recover(&runner, request.clone()).unwrap();
    assert!(
        restarted
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    std::fs::set_permissions(
        request.target_home.join(".local"),
        std::fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(recover(&runner, request).is_err());
    assert!(runner.calls.borrow().is_empty());
}

#[test]
fn flatpak_only_restart_before_subtree_workers_start_uses_original_retained_review() {
    let (_temp, mut request, runner) = fixture(false);
    request.copy_home = false;
    request.copy_flatpak = true;
    std::fs::create_dir_all(request.source_home.join(".local/share/flatpak/overrides")).unwrap();
    std::fs::write(
        request
            .source_home
            .join(".local/share/flatpak/overrides/global"),
        b"original permissions",
    )
    .unwrap();
    let reviewed = review(&runner, request.clone()).unwrap();
    for copy in &reviewed.copies {
        copy.retain_review().unwrap();
    }
    durable::save(&request).unwrap();
    drop(reviewed);
    let restarted = recover(&runner, request.clone()).unwrap();
    assert!(
        restarted
            .execute(&runner, &AtomicBool::new(false), true, |_| {})
            .unwrap()
            .complete()
    );
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".local/share/flatpak/overrides/global")
        )
        .unwrap(),
        b"original permissions"
    );
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
    assert!(!request.target_home.join("personal").exists());
    assert!(runner.calls.borrow().is_empty());
}

fn adoption_fixture_home(request: &TransferRequest) -> (PathBuf, PathBuf) {
    let home = request.target_home.parent().unwrap().join("active-home");
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    for relative in [".config/tilo", ".local/state/tilo", ".local/share"] {
        std::fs::create_dir_all(home.join(relative)).unwrap();
        std::fs::set_permissions(home.join(relative), std::fs::Permissions::from_mode(0o700))
            .unwrap();
    }
    let journal = request.private_journals.join("adoption");
    (home, journal)
}

fn source_user_repository(request: &TransferRequest) {
    let repository = request.source_home.join(".local/share/flatpak/repo");
    std::fs::create_dir_all(repository.join("objects")).unwrap();
    std::fs::write(
        repository.join("objects/original-user-object"),
        b"original user object",
    )
    .unwrap();
    std::fs::write(
        repository.join("user.trustedkeys.gpg"),
        b"original trusted keys",
    )
    .unwrap();
    std::fs::write(repository.join("config"), b"[core]\nmode=bare-user-only\n").unwrap();
}

#[test]
fn partial_adoption_revalidates_relocated_signed_apps_original_objects_and_userdata() {
    let (_temp, request, runner) = fixture(true);
    source_user_repository(&request);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let (active, journal) = adoption_fixture_home(&request);
    let promotion = reviewed.review_promotion(&active, &journal).unwrap();
    promotion
        .move_one_for_test(Path::new(".local/share/flatpak"))
        .unwrap();
    assert!(!request.target_home.join(".local/share/flatpak").exists());
    assert!(
        request
            .target_home
            .join(".var/app/org.example.App/data/session")
            .exists()
    );
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    let mapped = FixtureRunner {
        replies: RefCell::new(fresh.replies.borrow_mut().drain(..5).collect()),
        calls: RefCell::default(),
        target: active.clone(),
    };
    let (_, recovered) =
        recover_promotion_with_runner(&mapped, request.clone(), &active, &journal).unwrap();
    assert!(
        !mapped
            .calls
            .borrow()
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("install"))
    );
    let mut worker = recovered.start(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = worker.try_next() {
            match event {
                super::super::home_transfer::promotion::PromotionEvent::Finished(report) => {
                    assert!(report.complete(), "{report:?}")
                }
                super::super::home_transfer::promotion::PromotionEvent::Failed(error) => {
                    panic!("{error}")
                }
            }
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        std::fs::read(active.join(".var/app/org.example.App/data/session")).unwrap(),
        b"session"
    );
    assert_eq!(
        std::fs::read(active.join(".local/share/flatpak/repo/objects/original-user-object"))
            .unwrap(),
        b"original user object"
    );
    assert_eq!(
        std::fs::read(active.join(".local/share/flatpak/repo/user.trustedkeys.gpg")).unwrap(),
        b"original trusted keys"
    );
    assert_eq!(
        std::fs::read(
            request
                .source_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
}

#[test]
fn partial_adoption_refuses_changed_application_output_without_reinstall_or_overwrite() {
    let (_temp, request, runner) = fixture(true);
    source_user_repository(&request);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let (active, journal) = adoption_fixture_home(&request);
    let promotion = reviewed.review_promotion(&active, &journal).unwrap();
    promotion
        .move_one_for_test(Path::new(".local/share/flatpak"))
        .unwrap();
    let witness = promotion.witness().unwrap();
    assert_eq!(witness.application_home().unwrap(), active);
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    let mapped = FixtureRunner {
        replies: RefCell::new(fresh.replies.borrow_mut().drain(..5).collect()),
        calls: RefCell::default(),
        target: active.clone(),
    };
    let (remotes, failures) = storage::prepare_remotes(&mapped, &request).unwrap();
    assert!(failures.is_empty());
    let approved = storage::publish_remotes(&request.private_journals, &remotes).unwrap();
    // First qualify the real mapped repository and exact ref context, so the
    // later refusal cannot be caused by missing fixture replies or wrong home.
    applications::verify_adopted_output(
        &request,
        witness.application_home().unwrap(),
        &mapped,
        &approved,
    )
    .unwrap();
    let object = active.join(".local/share/flatpak/repo/fixture-object");
    std::fs::write(&object, b"altered output").unwrap();
    let calls = mapped.calls.borrow().len();
    let error = applications::verify_adopted_output(
        &request,
        witness.application_home().unwrap(),
        &mapped,
        &approved,
    )
    .unwrap_err();
    assert_eq!(
        error,
        "Relocated application output differs from the verified signed transaction"
    );
    assert_eq!(mapped.calls.borrow().len(), calls);
    assert!(recover_promotion_with_runner(&mapped, request.clone(), &active, &journal).is_err());
    assert_eq!(mapped.calls.borrow().len(), calls);
    assert_eq!(std::fs::read(&object).unwrap(), b"altered output");
    assert_eq!(
        std::fs::read(
            request
                .target_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
    assert_eq!(
        std::fs::read(
            request
                .source_home
                .join(".local/share/flatpak/repo/objects/original-user-object")
        )
        .unwrap(),
        b"original user object"
    );
}
#[test]
fn app_only_partial_adoption_recovers_moved_data_without_copying_personal_home_or_identity() {
    let (_temp, mut request, runner) = fixture(false);
    request.copy_home = false;
    request.copy_flatpak = true;
    source_user_repository(&request);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    assert!(!request.target_home.join("personal").exists());
    let (active, journal) = adoption_fixture_home(&request);
    std::fs::write(
        active.join(".config/tilo/selected-identity"),
        b"selected new identity",
    )
    .unwrap();
    let promotion = reviewed.review_promotion(&active, &journal).unwrap();
    promotion.move_one_for_test(Path::new(".var")).unwrap();
    assert!(!request.target_home.join(".var").exists());
    let (_, recovered) =
        recover_promotion_with_runner(&runner, request.clone(), &active, &journal).unwrap();
    let mut worker = recovered.start(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = worker.try_next() {
            match event {
                super::super::home_transfer::promotion::PromotionEvent::Finished(report) => {
                    assert!(report.complete(), "{report:?}")
                }
                super::super::home_transfer::promotion::PromotionEvent::Failed(error) => {
                    panic!("{error}")
                }
            }
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        std::fs::read(active.join(".var/app/org.example.App/data/session")).unwrap(),
        b"session"
    );
    assert_eq!(
        std::fs::read(active.join(".local/share/flatpak/repo/objects/original-user-object"))
            .unwrap(),
        b"original user object"
    );
    assert_eq!(
        std::fs::read(active.join(".config/tilo/selected-identity")).unwrap(),
        b"selected new identity"
    );
    assert_eq!(
        std::fs::read(request.source_home.join("personal")).unwrap(),
        b"personal"
    );
    assert!(!active.join("personal").exists());
    assert!(runner.calls.borrow().is_empty());
}
#[test]
fn app_only_promotion_refuses_unreviewed_home_data_and_changed_original_subtrees() {
    let (_temp, mut request, runner) = fixture(false);
    request.copy_home = false;
    request.copy_flatpak = true;
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let (active, journal) = adoption_fixture_home(&request);
    std::fs::write(
        request.target_home.join("personal"),
        b"unreviewed home file",
    )
    .unwrap();
    assert!(reviewed.review_promotion(&active, &journal).is_err());
    std::fs::remove_file(request.target_home.join("personal")).unwrap();
    std::fs::write(
        request
            .target_home
            .join(".var/app/org.example.App/data/session"),
        b"changed imported session",
    )
    .unwrap();
    assert!(reviewed.review_promotion(&active, &journal).is_err());
    assert_eq!(
        std::fs::read(
            request
                .source_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
    assert!(!journal.exists());
}

#[test]
fn app_only_partial_adoption_revalidates_signed_output_and_original_subtree_receipts() {
    let (_temp, mut request, runner) = fixture(true);
    request.copy_home = false;
    source_user_repository(&request);
    let reviewed = review(&runner, request.clone()).unwrap();
    assert!(
        reviewed
            .execute(&runner, &AtomicBool::new(false), false, |_| {})
            .unwrap()
            .complete()
    );
    let (active, journal) = adoption_fixture_home(&request);
    let promotion = reviewed.review_promotion(&active, &journal).unwrap();
    promotion
        .move_one_for_test(Path::new(".local/share/flatpak"))
        .unwrap();
    assert!(!request.target_home.join(".local/share/flatpak").exists());
    assert!(
        request
            .target_home
            .join(".var/app/org.example.App/data/session")
            .exists()
    );
    let (_fresh_temp, _fresh_request, fresh) = fixture(true);
    let mapped = FixtureRunner {
        replies: RefCell::new(fresh.replies.borrow_mut().drain(..5).collect()),
        calls: RefCell::default(),
        target: active.clone(),
    };
    let (_, recovered) =
        recover_promotion_with_runner(&mapped, request.clone(), &active, &journal).unwrap();
    assert!(
        !mapped
            .calls
            .borrow()
            .iter()
            .any(|args| args.first().map(String::as_str) == Some("install"))
    );
    let mut worker = recovered.start(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(event) = worker.try_next() {
            match event {
                super::super::home_transfer::promotion::PromotionEvent::Finished(report) => {
                    assert!(report.complete(), "{report:?}")
                }
                super::super::home_transfer::promotion::PromotionEvent::Failed(error) => {
                    panic!("{error}")
                }
            }
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        std::fs::read(active.join(".var/app/org.example.App/data/session")).unwrap(),
        b"session"
    );
    assert_eq!(
        std::fs::read(active.join(".local/share/flatpak/repo/objects/original-user-object"))
            .unwrap(),
        b"original user object"
    );
    assert_eq!(
        std::fs::read(active.join(".local/share/flatpak/repo/user.trustedkeys.gpg")).unwrap(),
        b"original trusted keys"
    );
    assert_eq!(
        std::fs::read(
            request
                .source_home
                .join(".var/app/org.example.App/data/session")
        )
        .unwrap(),
        b"session"
    );
}
