// Guided setup driven as a person or an agent drives it: every step of the
// internal installation and the Desktop journey goes through the host's
// published effects and rows, over the fake platform, a local release server
// and the staging helper's scripted runs. No disk, firmware or network.

use tilo_actions::ApiError;

/// A setup mounted in the host, with the fake platform and its release server.
struct Hosted {
    app: TestHost,
    fakes: install_flow::tests::Fakes,
    server: Server,
    bytes: Vec<u8>,
    _root: tempfile::TempDir,
    _preference: tempfile::TempDir,
}

fn mount(fixture: HeldFixture) -> Hosted {
    Hosted {
        app: launch(fixture.app),
        fakes: fixture.fakes,
        server: fixture._server,
        bytes: fixture.bytes,
        _root: fixture._root,
        _preference: fixture._preference,
    }
}

/// Guided setup over `server`, with `serves` released selections offered in
/// turn to each download the journey starts, resumes or discards.
fn setup_fixture(
    server: Server,
    fakes: install_flow::tests::Fakes,
    host: HostOs,
    serves: usize,
) -> HeldFixture {
    setup_fixture_in(server, fakes, host, serves, None)
}

fn setup_fixture_with_runtime(
    server: Server,
    fakes: install_flow::tests::Fakes,
    host: HostOs,
    serves: usize,
    runtime: install_flow::StagingRuntime,
) -> HeldFixture {
    setup_fixture_in(server, fakes, host, serves, Some(runtime))
}

fn setup_fixture_in(
    server: Server,
    fakes: install_flow::tests::Fakes,
    host: HostOs,
    serves: usize,
    runtime: Option<install_flow::StagingRuntime>,
) -> HeldFixture {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let root = tempfile::tempdir().unwrap();
    let preference = tempfile::tempdir().unwrap();
    let capabilities = SetupCapabilities {
        platform: Capabilities {
            host,
            desktop_session: true,
            internal_dual_boot: false,
        },
    };
    let runtime = runtime.unwrap_or_else(|| fakes.runtime());
    let install = InternalInstall::new(runtime, true, host);
    let worker = Worker::fixture_for_test(
        root.path(),
        server.transport(),
        install_flow::staging_consumer(install.link()),
    );
    let mut app = SetupApp::with_parts(capabilities, run_inline, install);
    app.internal_download_flow = Some(InternalDownloadFlow::fixture_at(
        capabilities.platform,
        preference.path().join("selection.json"),
        worker,
        (0..serves).map(|_| vec![server.release(&bytes)]).collect(),
        "x86_64",
    ));
    HeldFixture {
        app,
        fakes,
        bytes,
        _server: server,
        _root: root,
        _preference: preference,
    }
}

/// What the setup publishes right now, read the way the palette or an agent
/// reads it.
fn setup_value(app: &mut TestHost) -> serde_json::Value {
    app.effect(&SetupEffect::ReadSetupStatus)
        .expect("the setup answers its status")
        .value
        .expect("the setup status is a projection")
}

fn internal_value(app: &mut TestHost) -> serde_json::Value {
    setup_value(app)["internal"].clone()
}

fn download_phase(app: &mut TestHost) -> String {
    internal_value(app)["download"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn download_failure(app: &mut TestHost) -> Option<String> {
    app.with_widget::<SetupApp, _>(|setup| {
        setup
            .internal_download_flow
            .as_ref()
            .and_then(|flow| flow.presentation().failure.clone())
    })
    .flatten()
}

fn wait_download(app: &mut TestHost, wanted: &[&str]) {
    tilo_test_env::wait_until(format!("the download to be one of {wanted:?}"), || {
        let phase = download_phase(app);
        assert!(
            phase != "failed" || wanted.contains(&"failed"),
            "the download failed instead of becoming {wanted:?}: {:?}",
            download_failure(app)
        );
        wanted.contains(&phase.as_str())
    });
}

fn wait_run(app: &mut TestHost, wanted: &str) {
    tilo_test_env::wait_until(format!("the installer run to be {wanted}"), || {
        internal_value(app)["run"].as_str() == Some(wanted)
    });
}

/// Start, pause, resume, release, discard and remove as the internal row
/// offers them: the row binds the reviewed digest and the pending operation,
/// so the caller names the command alone.
fn internal_effect(
    app: &mut TestHost,
    effect: &SetupEffect,
) -> Result<tilo_actions::ActionOutput, ApiError> {
    app.effect_on("imager.setup.internal", effect)
}

/// The fixture release, served by what each request asks for.
fn release_server(bytes: &[u8]) -> Server {
    Server::serve_release(bytes.to_vec(), vec![b'E'; 4096], vec![b'R'; 8192], |_| {})
}

/// A release server whose first component request waits until the test lets
/// it go, so the download sits in the phase a person can pause or cancel.
fn held_release_server(bytes: &[u8]) -> (Server, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
    let (blocked_tx, blocked_rx) = mpsc::sync_channel(0);
    let (go_tx, go_rx) = mpsc::sync_channel(0);
    let mut held = false;
    let server = Server::serve_release(
        bytes.to_vec(),
        vec![b'E'; 4096],
        vec![b'R'; 8192],
        move |target| {
            if !held && target.ends_with("staging-esp.img") {
                held = true;
                blocked_tx.send(()).expect("the test waits for the block");
                go_rx.recv().expect("the test lets the request go");
            }
        },
    );
    (server, blocked_rx, go_tx)
}

fn internal_sentence(key: &str) -> String {
    crate::setup::strings::t(key)
}

#[test]
fn journey_the_review_checks_this_computer_before_offering_the_full_os_install() {
    use crate::setup::staging::probe::Blocker;
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());

    let mut fixture = setup_fixture(
        release_server(&bytes),
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        2,
    );
    review(&mut fixture.app, Experience::FullOs);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;
    assert_eq!(internal_value(app)["readiness"], "checking");
    let text = app
        .with_widget::<SetupApp, _>(|setup| setup.internal_readiness_text())
        .unwrap();
    assert!(
        text.starts_with("Checking whether Tilo can be installed"),
        "{text}"
    );

    internal_effect(app, &SetupEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the check to answer", || {
        app.settle();
        app.with_widget::<SetupApp, _>(|setup| !setup.internal_install.is_probing())
            .unwrap()
    });
    assert_eq!(internal_value(app)["readiness"], "ready");
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 1);

    // An older MBR disk is not offered: the person is told why, and no
    // install is published.
    let mut older_disk = setup_fixture(
        release_server(&bytes),
        install_flow::tests::Fakes::new(vec![Blocker::NotGpt]),
        HostOs::Linux,
        2,
    );
    review(&mut older_disk.app, Experience::FullOs);
    let mut refused_setup = mount(older_disk);
    let app = &mut refused_setup.app;
    internal_effect(app, &SetupEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the refused check to answer", || {
        app.settle();
        app.with_widget::<SetupApp, _>(|setup| !setup.internal_install.is_probing())
            .unwrap()
    });
    let refused = internal_value(app);
    assert_eq!(refused["readiness"], "unsupported");
    assert_eq!(refused["availability"]["install"], false);
    assert_eq!(refused["availability"]["start_download"], false);
    let text = app
        .with_widget::<SetupApp, _>(|setup| setup.internal_readiness_text())
        .unwrap();
    assert!(text.contains("MBR"), "{text}");
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.install-unavailable",
        ))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named("setup.internal.install"))
            .is_err(),
        "no install is offered for a disk Tilo cannot share"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey keeps each step and its fresh fixture visible"
)]
fn journey_a_full_os_download_pauses_resumes_and_installs_alongside_the_current_system() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let (server, blocked_rx, go_tx) = held_release_server(&bytes);
    let mut fixture = setup_fixture(
        server,
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        6,
    );
    open_full_review(&mut fixture.app);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;

    let status = internal_value(app);
    assert_eq!(status["download"], "none");
    assert_eq!(status["availability"]["start_download"], true);
    assert_eq!(status["availability"]["install"], false);
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.install").enabled(false))
        .unwrap();

    internal_effect(app, &SetupEffect::StartInternalDownload).unwrap();
    tilo_test_env::recv("the download to reach its first component", &blocked_rx);
    wait_download(
        app,
        &["authenticating", "acquiring", "preparing", "downloading"],
    );
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.pause").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.cancel").enabled(true))
        .unwrap();

    internal_effect(app, &SetupEffect::PauseInternalDownload).unwrap();
    go_tx
        .send(())
        .expect("the server finishes the held request");
    wait_download(app, &["paused"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-paused")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.resume").enabled(true))
        .unwrap();
    assert_eq!(internal_value(app)["availability"]["install"], false);

    internal_effect(app, &SetupEffect::ResumeInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-ready")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.release").enabled(true))
        .unwrap();
    assert_eq!(internal_value(app)["availability"]["install"], true);

    // A caller holding the review before a size change is refused: the plan
    // it reviewed is not the plan the installer would run.
    let stale = internal_value(app)["reviewed"]
        .as_str()
        .expect("the review is published")
        .to_owned();
    app.effect(&SetupEffect::SetInternalTargetSize { gib: 100 })
        .unwrap();
    assert_eq!(internal_value(app)["target_gib"], 100);
    let out_of_range = app
        .effect(&SetupEffect::SetInternalTargetSize { gib: 10 })
        .unwrap_err();
    assert!(
        format!("{out_of_range:?}").contains("Choose a Tilo size between"),
        "{out_of_range:?}"
    );
    assert_eq!(internal_value(app)["target_gib"], 100);
    let stale_refusal = app
        .effect(&SetupEffect::InstallInternal { reviewed: stale })
        .unwrap_err();
    assert!(
        format!("{stale_refusal:?}").contains(&crate::setup::strings::t(
            "imager-setup-internal-error-plan-changed"
        )),
        "{stale_refusal:?}"
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());

    // Installing through the row binds the digest the review published.
    internal_effect(
        app,
        &SetupEffect::InstallInternal {
            reviewed: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    wait_download(app, &["consumed"]);
    let installed = internal_value(app);
    assert_eq!(installed["run_action"], "stage");
    assert_eq!(installed["availability"]["restart"], true);
    assert_eq!(installed["availability"]["restart_later"], true);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run")
                .name_contains("stays the startup default"),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.restart").enabled(true))
        .unwrap();
    assert!(
        internal_effect(
            app,
            &SetupEffect::InstallInternal {
                reviewed: String::new()
            }
        )
        .is_err(),
        "a staged installer cannot be staged again"
    );
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].action, HelperAction::Stage);
        assert_eq!(launches[0].plan_sha256, launches[0].plan.digest().unwrap());
        assert_eq!(hosted.fakes.manifests.lock().unwrap()[0], hosted.bytes);
    }

    internal_effect(app, &SetupEffect::RestartInternalLater).unwrap();
    assert_eq!(internal_value(app)["availability"]["restart_later"], false);
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named("setup.internal.restart-later"))
            .is_err(),
        "a deferred restart is not offered twice"
    );
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run").name_contains(
                crate::setup::strings::t("imager-setup-internal-restart-deferred"),
            ),
        )
        .unwrap();

    internal_effect(app, &SetupEffect::RestartInternal).unwrap();
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::Restart]
    );
    assert!(
        hosted.server.requests().len() >= 5,
        "the resumed download refetched"
    );
}

#[test]
fn journey_an_internal_download_can_be_cancelled_and_its_data_discarded() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let (server, blocked_rx, go_tx) = held_release_server(&bytes);
    let mut fixture = setup_fixture(
        server,
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Linux,
        4,
    );
    open_full_review(&mut fixture.app);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;

    internal_effect(app, &SetupEffect::StartInternalDownload).unwrap();
    tilo_test_env::recv("the download to reach its first component", &blocked_rx);
    wait_download(app, &["downloading", "acquiring", "preparing"]);
    internal_effect(app, &SetupEffect::CancelInternalDownload).unwrap();
    go_tx
        .send(())
        .expect("the server finishes the held request");
    wait_download(app, &["cancelled"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-cancelled")),
        )
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.discard").enabled(true))
        .unwrap();

    internal_effect(app, &SetupEffect::DiscardInternalDownload).unwrap();
    wait_download(app, &["discarded"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-discarded")),
        )
        .unwrap();
    assert_eq!(
        internal_value(app)["availability"]["discard_download"],
        false
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());
}

#[test]
fn journey_a_verified_internal_download_is_kept_for_later_and_can_still_be_discarded() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let server = release_server(&bytes);
    let mut fixture = setup_fixture(
        server,
        install_flow::tests::Fakes::new(Vec::new()),
        HostOs::Windows,
        4,
    );
    open_full_review(&mut fixture.app);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;

    internal_effect(app, &SetupEffect::StartInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    internal_effect(app, &SetupEffect::ReleaseInternalDownload).unwrap();
    wait_download(app, &["released"]);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.status")
                .name_contains(internal_sentence("imager-download-sentence-released")),
        )
        .unwrap();
    let kept = internal_value(app);
    assert_eq!(kept["availability"]["resume_download"], true);
    assert_eq!(kept["availability"]["discard_download"], true);

    internal_effect(app, &SetupEffect::DiscardInternalDownload).unwrap();
    wait_download(app, &["discarded"]);
    assert_eq!(internal_value(app)["availability"]["install"], false);
}

/// A Full Tilo review whose probe answers with `blockers`, and whose Fast
/// Startup fix clears them, as a person moving through firmware would find.
fn remedy_fixture(blockers: Vec<crate::setup::staging::probe::Blocker>) -> Hosted {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let fakes = install_flow::tests::Fakes::new(blockers);
    let facts = Arc::new(std::sync::Mutex::new(fakes.facts.clone()));
    let mut runtime = fakes.runtime();
    let probes = Arc::clone(&fakes.probes);
    let shared = Arc::clone(&facts);
    runtime.probe = Arc::new(move || {
        probes.fetch_add(1, Ordering::SeqCst);
        shared.lock().unwrap().clone()
    });
    // Turning Fast Startup off is the fix itself: once the command runs, the
    // computer's next check finds the blockers gone, as after a real change.
    let commands = Arc::clone(&fakes.commands);
    let shared = Arc::clone(&facts);
    runtime.host_command = Arc::new(move |command| {
        if command == HostCommand::DisableFastStartup {
            shared.lock().unwrap().blockers.clear();
        }
        commands.lock().unwrap().push(command);
        Ok(())
    });
    let mut fixture =
        setup_fixture_with_runtime(release_server(&bytes), fakes, HostOs::Windows, 4, runtime);
    open_full_review(&mut fixture.app);
    mount(fixture)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey keeps each remedy and the recheck after it visible"
)]
fn journey_a_refused_full_os_install_opens_firmware_settings_disables_fast_startup_and_rechecks() {
    use crate::setup::staging::probe::Blocker;
    let mut hosted = remedy_fixture(vec![Blocker::SecureBootOn, Blocker::Hibernation]);
    let app = &mut hosted.app;

    let blocked = internal_value(app);
    assert_eq!(blocked["availability"]["install"], false);
    assert_eq!(blocked["availability"]["firmware_settings"], true);
    assert_eq!(blocked["availability"]["disable_fast_startup"], true);
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.secure-boot-on",
        ))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.hibernation",
        ))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.firmware-settings").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.disable-fast-startup").enabled(true))
        .unwrap();
    assert!(
        internal_effect(
            app,
            &SetupEffect::InstallInternal {
                reviewed: String::new()
            }
        )
        .is_err()
    );

    internal_effect(app, &SetupEffect::InternalFirmwareSettings).unwrap();
    tilo_test_env::wait_until("firmware settings to open", || {
        app.settle();
        app.with_widget::<SetupApp, _>(|setup| setup.host_command_pending.is_none())
            .unwrap()
    });
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::FirmwareSettings {
            suspend_bitlocker: false
        }]
    );

    // The person turns Secure Boot off in firmware, then lets setup turn off
    // Fast Startup; its success rechecks this computer by itself.
    internal_effect(app, &SetupEffect::InternalDisableFastStartup).unwrap();
    tilo_test_env::wait_until("the recheck after Fast Startup", || {
        app.settle();
        app.with_widget::<SetupApp, _>(|setup| {
            setup.host_command_pending.is_none() && !setup.internal_install.is_probing()
        })
        .unwrap()
    });
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![
            HostCommand::FirmwareSettings {
                suspend_bitlocker: false
            },
            HostCommand::DisableFastStartup
        ]
    );
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 2);
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.blockers"))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named(
                "setup.internal.blocker.hibernation",
            ))
            .is_err()
    );

    internal_effect(app, &SetupEffect::RecheckInternal).unwrap();
    tilo_test_env::wait_until("the recheck to answer", || {
        app.settle();
        app.with_widget::<SetupApp, _>(|setup| !setup.internal_install.is_probing())
            .unwrap()
    });
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 3);
    let ready = internal_value(app);
    assert_eq!(ready["readiness"], "ready");
    assert_eq!(ready["availability"]["firmware_settings"], false);
    assert_eq!(ready["availability"]["disable_fast_startup"], false);
    // Still no install: the verified download comes first.
    assert_eq!(ready["availability"]["install"], false);

    let mut battery = remedy_fixture(vec![Blocker::OnBattery]);
    let app = &mut battery.app;
    let state = internal_value(app);
    assert_eq!(
        state["availability"]["firmware_settings"], false,
        "battery is not a firmware problem"
    );
    app.harness()
        .expect(&SemanticExpectation::named(
            "setup.internal.blocker.on-battery",
        ))
        .unwrap();
    assert!(
        app.harness()
            .expect(&SemanticExpectation::named(
                "setup.internal.firmware-settings",
            ))
            .is_err()
    );
    assert!(internal_effect(app, &SetupEffect::InternalFirmwareSettings).is_err());
    assert!(battery.fakes.commands.lock().unwrap().is_empty());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey keeps the failed run and its retry visible"
)]
fn journey_a_failed_internal_install_retries_the_unfinished_operation_to_a_staged_install() {
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let mut fakes = install_flow::tests::Fakes::new(vec![
        crate::setup::staging::probe::Blocker::PendingOperation,
    ]);
    fakes.records = vec![install_flow::tests::record("op-1")];
    let runtime = queued_runtime(
        &fakes,
        vec![
            vec![install_flow::tests::finished(false, Some("boot-next-busy"))],
            vec![install_flow::tests::finished(true, None)],
        ],
    );
    let mut fixture =
        setup_fixture_with_runtime(release_server(&bytes), fakes, HostOs::Windows, 4, runtime);
    open_full_review(&mut fixture.app);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;

    internal_effect(app, &SetupEffect::StartInternalDownload).unwrap();
    wait_download(app, &["ready"]);
    assert_eq!(internal_value(app)["pending_operation"], "op-1");
    internal_effect(
        app,
        &SetupEffect::InstallInternal {
            reviewed: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "failed");
    let failed = internal_value(app);
    assert_eq!(failed["failure"], "boot-next-busy");
    assert_eq!(failed["availability"]["retry"], true);
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run").name_contains(internal_sentence(
                "imager-setup-internal-failure-boot-next-busy",
            )),
        )
        .unwrap();

    internal_effect(
        app,
        &SetupEffect::RetryInternal {
            operation_id: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    let staged = internal_value(app);
    assert_eq!(staged["run_action"], "arm");
    assert_eq!(staged["availability"]["restart"], true);
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(
            launches
                .iter()
                .map(|request| (request.action, request.operation_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(HelperAction::Stage, "op-1"), (HelperAction::Arm, "op-1")]
        );
    }

    internal_effect(app, &SetupEffect::RestartInternalLater).unwrap();
    internal_effect(app, &SetupEffect::RestartInternal).unwrap();
    assert_eq!(
        *hosted.fakes.commands.lock().unwrap(),
        vec![HostCommand::Restart]
    );
}

#[test]
fn journey_an_unfinished_internal_install_is_removed_and_its_space_returned() {
    use crate::setup::staging::probe::Blocker;
    let bytes = internal_bundle::tests::bytes(&internal_bundle::tests::fixture());
    let mut fakes = install_flow::tests::Fakes::new(vec![Blocker::PendingOperation]);
    fakes.records = vec![install_flow::tests::record("op-1")];
    let mut fixture = setup_fixture(release_server(&bytes), fakes, HostOs::Linux, 4);
    open_full_review(&mut fixture.app);
    let mut hosted = mount(fixture);
    let app = &mut hosted.app;

    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.retry").enabled(true))
        .unwrap();
    app.harness()
        .expect(&SemanticExpectation::named("setup.internal.remove").enabled(true))
        .unwrap();
    let stale = app
        .effect(&SetupEffect::RemoveInternal {
            operation_id: "op-0".into(),
        })
        .unwrap_err();
    assert!(
        format!("{stale:?}").contains(&crate::setup::strings::t(
            "imager-setup-internal-error-pending-changed"
        )),
        "{stale:?}"
    );
    assert!(hosted.fakes.launches.lock().unwrap().is_empty());
    internal_effect(
        app,
        &SetupEffect::RemoveInternal {
            operation_id: String::new(),
        },
    )
    .unwrap();
    wait_run(app, "succeeded");
    let removed = internal_value(app);
    assert_eq!(removed["run_action"], "remove");
    app.harness()
        .expect(
            &SemanticExpectation::named("setup.internal.run")
                .name_contains(internal_sentence("imager-setup-internal-removed")),
        )
        .unwrap();
    {
        let launches = hosted.fakes.launches.lock().unwrap();
        assert_eq!(
            launches
                .iter()
                .map(|request| (request.action, request.operation_id.as_str()))
                .collect::<Vec<_>>(),
            vec![(HelperAction::Remove, "op-1")]
        );
    }
    assert_eq!(hosted.fakes.probes.load(Ordering::SeqCst), 2);
}

/// The staging helper's runs, one script per launch: the first attempt fails
/// the way a busy one-time startup choice does, the next finishes.
fn queued_runtime(
    fakes: &install_flow::tests::Fakes,
    scripts: Vec<Vec<crate::setup::staging::helper::Poll>>,
) -> install_flow::StagingRuntime {
    let mut runtime = fakes.runtime();
    let queue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        scripts,
    )));
    let launches = Arc::clone(&fakes.launches);
    let manifests = Arc::clone(&fakes.manifests);
    runtime.launch = Arc::new(move |request: &crate::setup::staging::helper::Request| {
        if let Ok(bytes) = std::fs::read(&request.manifest_path) {
            manifests.lock().unwrap().push(bytes);
        }
        launches.lock().unwrap().push(request.clone());
        let script = queue.lock().unwrap().pop_front().unwrap_or_default();
        Ok(Box::new(install_flow::tests::ScriptedHelper(script.into()))
            as Box<dyn install_flow::HelperRun>)
    });
    runtime
}
