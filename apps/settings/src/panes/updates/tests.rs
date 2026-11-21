use super::*;
use std::sync::Arc;
use std::sync::Mutex;

fn test_cx() -> crate::sections::Sections {
    crate::sections::Sections::default()
}

/// One deployment of the system, as the session's reading names it.
fn deployment(version: &str, digest: &str) -> Deployment {
    Deployment {
        image: Some("registry.tilo.so/tilo/tilo:stable".into()),
        digest: Some(digest.into()),
        version: Some(version.into()),
        built: Some("2026-06-02T09:00:00Z".into()),
    }
}

/// What the session reads of a machine that runs one version and keeps the one before.
fn reading() -> Update {
    Update {
        booted: Some(deployment("Tilo 2026.06", "sha256:06")),
        rollback: Some(deployment("Tilo 2026.05", "sha256:05")),
        ..Update::default()
    }
}

/// The same machine with the next version staged, which a restart of the computer finishes.
fn staged() -> Update {
    Update {
        staged: Some(deployment("Tilo 2026.07", "sha256:07")),
        restart_needed: true,
        ..reading()
    }
}

/// A pane reading `update` from the session, asking the session `host`, driving `control`.
fn pane_reading(
    update: &Update,
    host: &Arc<SessionHost>,
    control: Arc<dyn UpdateControl>,
) -> (UpdatesModel, Arc<HeldReading>) {
    let feed = HeldReading::of(update);
    let pane = UpdatesModel::with_parts(
        test_cx(),
        UpdatePrefs::default(),
        Arc::clone(&feed) as Arc<dyn UpdateFeed>,
        Box::new(SessionActions::new(host.remote())),
        Arc::new(|| "CHECKED-NOW".to_string()),
        control,
    );
    (pane, feed)
}

fn pane() -> UpdatesModel {
    pane_reading(
        &reading(),
        &Arc::new(SessionHost::default()),
        Arc::new(UnavailableControl),
    )
    .0
}

/// The session as the pane's actions reach it: what it was asked, with the person's
/// confirmation, and a refusal where it gives one.
#[derive(Default)]
struct SessionHost {
    asked: Mutex<Vec<ui::tilo_actions::remote::InvokeRequest>>,
    refusal: Mutex<Option<String>>,
}

impl SessionHost {
    fn remote(self: &Arc<Self>) -> ui::RemoteActions {
        ui::RemoteActions::with_provider(
            Arc::clone(self) as Arc<dyn ui::tilo_actions::remote::ActionProvider + Send + Sync>,
            ui::tilo_actions::remote::Caller::local_person("owner"),
        )
    }

    /// What was asked: the resource, the action and whether the person confirmed it.
    fn asked(&self) -> Vec<(String, String, bool)> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|request| {
                assert_eq!(
                    request.target.provider,
                    ui::tilo_actions::remote::session_actions::PROVIDER
                );
                (
                    request.target.address.resource.clone(),
                    request.target.address.action.clone(),
                    request.confirmation == Some(ui::tilo_actions::remote::Confirmation::Confirmed),
                )
            })
            .collect()
    }

    fn refuse(&self, reason: &str) {
        *self.refusal.lock().unwrap() = Some(reason.to_owned());
    }
}

impl ui::tilo_actions::remote::ActionProvider for SessionHost {
    fn query(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: ui::tilo_actions::remote::CatalogueQuery,
    ) -> ui::tilo_actions::remote::Pending<
        Result<ui::tilo_actions::remote::CataloguePage, ui::tilo_actions::remote::ActionCallError>,
    > {
        ui::tilo_actions::remote::Pending::ready(Err(
            ui::tilo_actions::remote::ActionCallError::NotPermitted,
        ))
    }

    fn invoke(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        request: ui::tilo_actions::remote::InvokeRequest,
    ) -> ui::tilo_actions::remote::Pending<
        Result<
            ui::tilo_actions::remote::InvocationReceipt,
            ui::tilo_actions::remote::ActionCallError,
        >,
    > {
        use ui::tilo_actions::remote::{ActionCallError, InvocationReceipt, ReceiptStatus};
        let invocation = request.invocation;
        self.asked.lock().unwrap().push(request);
        let refused = self.refusal.lock().unwrap().clone();
        let status = match refused {
            Some(reason) => ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable { reason },
            },
            None => ReceiptStatus::Completed {
                output: ui::tilo_actions::ActionOutput {
                    value: None,
                    resources: Vec::new(),
                },
            },
        };
        ui::tilo_actions::remote::Pending::ready(Ok(InvocationReceipt { invocation, status }))
    }

    fn observe(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> Option<ui::tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &ui::tilo_actions::remote::Caller,
        _: &ui::tilo_actions::InvocationId,
    ) -> ui::tilo_actions::remote::Pending<Result<(), ui::tilo_actions::remote::ActionCallError>>
    {
        ui::tilo_actions::remote::Pending::ready(Ok(()))
    }
}

fn codec_pane(
    status: Arc<Mutex<String>>,
    runs: Arc<Mutex<Vec<CodecAction>>>,
    outcome: Result<(), String>,
) -> UpdatesModel {
    let reader = Arc::clone(&status);
    pane().with_codec_pack(
            Arc::new(move || CodecPackStatus::parse(&reader.lock().unwrap())),
            Arc::new(move |action| {
                runs.lock().unwrap().push(action);
                *status.lock().unwrap() = match (action, outcome.is_ok()) {
                    (CodecAction::Install | CodecAction::Refresh, true) => {
                        r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#
                    }
                    (CodecAction::Install | CodecAction::Refresh, false) => {
                        r#"{"state":"failed","last_error":"offline","desired":true}"#
                    }
                    (CodecAction::Remove, _) => r#"{"state":"not-installed","desired":false}"#,
                }
                .to_string();
                outcome.clone()
            }),
        )
}

fn shared_status(json: &str) -> Arc<Mutex<String>> {
    Arc::new(Mutex::new(json.to_string()))
}

fn press_and_finish(pane: &mut UpdatesModel, effect: UpdatesEffect) {
    pane.poll();
    pane.run(effect, None);
    pane.poll();
    pane.wait_for_codec_operation();
    pane.poll();
}

tilo_test_env::scenario! {
    fn the_codec_card_follows_the_helper;

    phase codec_card_is_one_short_line_and_one_button;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let pane = codec_pane(shared_status(""), runs, Ok(()));
        assert_eq!(
            pane.codecs_card_lines(),
            [
                "Play more video and audio formats",
                "Downloaded to this device from the formats' publishers. You can turn it off here at any time.",
                "Status: Off",
                "Install",
            ]
        );
    }

    phase every_helper_state_has_its_status_and_controls;
    {
        let cases: [(&str, &str, &[&str]); 7] = [
            (r#"{"state":"not-installed"}"#, "Off", &["Install"]),
            (
                r#"{"state":"waiting-for-network","desired":true}"#,
                "Waiting for an internet connection",
                &["Cancel"],
            ),
            (
                r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":7,"label":"Downloading"}}"#,
                "Downloading (step 3 of 7)",
                &[],
            ),
            (
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
                "On, including hardware H.264/HEVC",
                &["Remove"],
            ),
            (
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
                "follows once a matching driver is published",
                &["Check for hardware support again", "Remove"],
            ),
            (
                r#"{"state":"failed","last_error":"no route to host","desired":true}"#,
                "Could not install: no route to host",
                &["Try again", "Remove"],
            ),
            (r#"{"state":"removing"}"#, "Removing", &[]),
        ];
        for (json, status_line, controls) in cases {
            let runs = Arc::new(Mutex::new(Vec::new()));
            let pane = codec_pane(shared_status(json), runs, Ok(()));
            let lines = pane.codecs_card_lines();
            assert!(
                lines
                    .iter()
                    .any(|line| line.starts_with("Status: ") && line.contains(status_line)),
                "{json}: {lines:?}"
            );
            let buttons = pane.codec_buttons();
            let offered: Vec<&str> = buttons.iter().map(|b| b.0.as_str()).collect();
            assert_eq!(offered, controls, "{json}");
        }
    }

    phase a_running_install_shows_the_helpers_progress;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installing","desired":true,"progress":{"step":3,"steps":5,"label":"Downloading","bytes":20000000,"total":40000000}}"#,
        );
        let pane = codec_pane(status, runs, Ok(()));
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Progress: 50%".to_string()), "{lines:?}");
        assert!(
            lines.contains(&"Status: Downloading 20 of 40 MB (step 3 of 5)\u{2026}".to_string()),
            "{lines:?}"
        );
    }

    phase the_watcher_updates_the_card_while_the_helper_works;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(r#"{"state":"waiting-for-network","desired":true}"#);
        let mut pane = codec_pane(Arc::clone(&status), runs, Ok(()));
        pane.poll();
        *status.lock().unwrap() =
                r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#.to_string();
        pane.poll();
        assert_eq!(pane.codec_status().state, PackState::WaitingForNetwork);
        pane.codec_status_file_changed();
        pane.poll();
        assert_eq!(pane.codec_status().state, PackState::Installed);
        assert!(
            pane.codecs_card_lines()
                .iter()
                .any(|line| line.contains("Sign out and back in"))
        );
    }

    phase install_runs_the_unit_and_shows_the_waiting_mesa_part;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = codec_pane(shared_status(""), Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Install]);
        assert_eq!(pane.codec_status().state, PackState::Installed);
        let lines = pane.codecs_card_lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("follows once a matching driver is published"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("No restart is needed"))
        );
        assert_eq!(lines.last().map(String::as_str), Some("Remove"));
        assert_eq!(pane.notice(), None);
    }

    phase a_request_the_helper_has_not_picked_up_reads_as_starting;
    {
        let (release, hold) = channel::channel::<()>();
        let hold = Mutex::new(hold);
        let mut pane = pane().with_codec_pack(
            Arc::new(CodecPackStatus::default),
            Arc::new(move |_| {
                let _ = hold.lock().unwrap().recv();
                Ok(())
            }),
        );
        pane.poll();
        pane.run(UpdatesEffect::InstallCodecs, None);
        pane.poll();
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Status: Starting\u{2026}".to_string()));
        assert!(lines.contains(&"Progress: 0%".to_string()));
        assert!(
            !lines.iter().any(|line| line == "Install"),
            "no second request while one runs"
        );
        release.send(()).unwrap();
        pane.wait_for_codec_operation();
    }

    phase failed_install_surfaces_the_helper_error_and_offers_retry;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = codec_pane(shared_status(""), Arc::clone(&runs), Err("offline".into()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(
            pane.notice(),
            Some("The formats could not be changed: offline")
        );
        let lines = pane.codecs_card_lines();
        assert!(lines.contains(&"Status: Could not install: offline".to_string()));
        assert!(lines.contains(&"Try again".to_string()));
        press_and_finish(&mut pane, UpdatesEffect::InstallCodecs);
        assert_eq!(
            *runs.lock().unwrap(),
            vec![CodecAction::Install, CodecAction::Install]
        );
    }

    phase checking_again_for_the_mesa_build_runs_a_refresh;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
        );
        let mut pane = codec_pane(status, Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::RefreshCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Refresh]);
    }

    phase remove_returns_to_not_installed;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let status = shared_status(
            r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"enabled"},"desired":true}"#,
        );
        let mut pane = codec_pane(status, Arc::clone(&runs), Ok(()));
        press_and_finish(&mut pane, UpdatesEffect::RemoveCodecs);
        assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Remove]);
        assert_eq!(pane.codec_status().state, PackState::NotInstalled);
        assert_eq!(
            pane.codecs_card_lines().last().map(String::as_str),
            Some("Install")
        );
    }
}

fn windows_pane(
    readiness: Arc<Mutex<WindowsReadiness>>,
    runs: Arc<Mutex<Vec<WindowsSupportAction>>>,
    outcome: Result<(), String>,
    after: WindowsReadiness,
) -> UpdatesModel {
    let reader = Arc::clone(&readiness);
    let mut pane = pane();
    pane.windows_support = (reader.lock().unwrap()).clone();
    pane.windows_support_provider = Arc::new(move || (reader.lock().unwrap()).clone());
    pane.windows_support_runner = Arc::new(move |action| {
        runs.lock().unwrap().push(action);
        *readiness.lock().unwrap() = after.clone();
        outcome.clone()
    });
    pane
}

fn disclosure() -> crate::windows_compat::SizeDisclosure {
    crate::windows_compat::SizeDisclosure::parse(include_str!(
        "../../../../../os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json"
    ))
    .expect("the image's catalogue entry declares the size")
}

fn needs_install() -> WindowsReadiness {
    WindowsReadiness::NeedsInstall(disclosure())
}

fn press_windows(pane: &mut UpdatesModel, effect: UpdatesEffect) {
    pane.poll();
    pane.run(effect, None);
    while pane.windows_events.is_some() {
        pane.drain_windows_events();
        std::thread::yield_now();
    }
    pane.poll();
}

tilo_test_env::scenario! {
    fn the_windows_support_card_follows_its_install;

    phase windows_card_discloses_the_download_before_it_installs;
    {
        let pane = windows_pane(
            Arc::new(Mutex::new(needs_install())),
            Arc::new(Mutex::new(Vec::new())),
            Ok(()),
            WindowsReadiness::Ready,
        );
        let lines = pane.windows_support_card_lines();
        assert_eq!(lines.first().map(String::as_str), Some("Run Windows apps"));
        assert!(
            lines.iter().any(|line| line
                .starts_with("Status: Not installed. Windows support is a separate download")
                && line.contains(&disclosure().sentence())),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("Install"));
    }

    phase installing_windows_support_runs_the_extension_and_shows_its_progress;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = windows_pane(
            Arc::new(Mutex::new(needs_install())),
            Arc::clone(&runs),
            Ok(()),
            WindowsReadiness::Ready,
        );
        press_windows(&mut pane, UpdatesEffect::InstallWindowsSupport);
        assert_eq!(*runs.lock().unwrap(), vec![WindowsSupportAction::Install]);
        assert_eq!(pane.windows_support(), &WindowsReadiness::Ready);
        let lines = pane.windows_support_card_lines();
        assert!(
            lines.contains(&"Status: Installed".to_string()),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("Remove"));
        assert_eq!(pane.notice(), None);
    }

    phase a_running_windows_install_shows_the_helpers_step;
    {
        let pane = windows_pane(
            Arc::new(Mutex::new(WindowsReadiness::Installing(Some(
                crate::windows_compat::Progress {
                    step: 3,
                    steps: 5,
                    label: "Downloading".into(),
                    bytes: Some((20_000_000, 40_000_000)),
                },
            )))),
            Arc::new(Mutex::new(Vec::new())),
            Ok(()),
            WindowsReadiness::Ready,
        );
        let lines = pane.windows_support_card_lines();
        assert!(
            lines.contains(&"Status: Downloading 20 of 40 MB (step 3 of 5)\u{2026}".to_string()),
            "{lines:?}"
        );
        assert!(lines.contains(&"Progress: 50%".to_string()), "{lines:?}");
        assert!(
            !lines
                .iter()
                .any(|line| line == "Install" || line == "Remove"),
            "nothing to press while it works: {lines:?}"
        );
    }

    phase a_failed_windows_install_surfaces_the_error_and_offers_retry;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = windows_pane(
            Arc::new(Mutex::new(needs_install())),
            Arc::clone(&runs),
            Err("no route to host".into()),
            WindowsReadiness::Failed("no route to host".into(), disclosure()),
        );
        press_windows(&mut pane, UpdatesEffect::InstallWindowsSupport);
        assert_eq!(
            pane.notice(),
            Some("Windows support could not be changed: no route to host")
        );
        let lines = pane.windows_support_card_lines();
        assert!(
            lines.contains(&"Status: Could not install: no route to host".to_string()),
            "{lines:?}"
        );
        assert!(lines.contains(&"Try again".to_string()), "{lines:?}");
    }

    phase removing_windows_support_returns_to_the_download_offer;
    {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let mut pane = windows_pane(
            Arc::new(Mutex::new(WindowsReadiness::Ready)),
            Arc::clone(&runs),
            Ok(()),
            needs_install(),
        );
        press_windows(&mut pane, UpdatesEffect::RemoveWindowsSupport);
        assert_eq!(*runs.lock().unwrap(), vec![WindowsSupportAction::Remove]);
        assert_eq!(
            pane.windows_support_card_lines().last().map(String::as_str),
            Some("Install")
        );
    }

    phase a_cancelled_install_offers_only_starting_it_again;
    {
        let mut pane = windows_pane(
            Arc::new(Mutex::new(WindowsReadiness::WaitingForNetwork)),
            Arc::new(Mutex::new(Vec::new())),
            Ok(()),
            needs_install(),
        );
        let lines = pane.windows_support_card_lines();
        assert!(
            lines
                .iter()
                .any(|line| line.contains("once this device is online")),
            "{lines:?}"
        );
        assert_eq!(lines.last().map(String::as_str), Some("Cancel"));
        press_windows(&mut pane, UpdatesEffect::RemoveWindowsSupport);
        assert_eq!(
            pane.windows_support_card_lines().last().map(String::as_str),
            Some("Install")
        );
    }
}

#[test]
fn the_update_history_keeps_its_printable_lines() {
    let history = parse_update_history("2026-01-01 update complete\nline\u{0007} two\n");
    assert_eq!(history, ["2026-01-01 update complete", "line two"]);
    assert!(parse_update_history("-- No entries --\n").is_empty());
}

/// The Updates page of a Settings window whose pane reads `update` from the session.
fn page_reading(update: &Update) -> String {
    page_of(|sections| pane_reading_in(sections, HeldReading::of(update)))
}

fn pane_reading_in(sections: crate::sections::Sections, feed: Arc<HeldReading>) -> UpdatesModel {
    UpdatesModel::with_parts(
        sections,
        UpdatePrefs::default(),
        feed,
        Box::new(SessionActions::new(
            Arc::new(SessionHost::default()).remote(),
        )),
        Arc::new(|| "CHECKED-NOW".to_string()),
        Arc::new(FakeControl::new(Some(true))),
    )
}

/// The Updates page of a Settings window whose pane `pane` makes.
fn page_of(pane: impl FnOnce(crate::sections::Sections) -> UpdatesModel) -> String {
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        *crate::panes::updates::area(settings)
            .unwrap()
            .model()
            .borrow_mut() = pane(settings.sections().clone());
    })
    .expect("the Settings window is mounted");
    crate::effects::show_pane(&mut app, crate::categories::Category::Updates);
    app.settle();
    app.harness().inspection_json().unwrap()
}

fn shows(frame: &str, shown: &[String], not: &[String]) {
    for claim in shown {
        assert!(frame.contains(claim.as_str()), "shows {claim:?}: {frame}");
    }
    for claim in not {
        assert!(
            !frame.contains(claim.as_str()),
            "does not show {claim:?}: {frame}"
        );
    }
}

tilo_test_env::scenario! {
    fn the_page_says_what_the_system_update_needs;

    phase nothing_to_do_offers_a_check_and_says_what_runs;
    {
        let frame = page_reading(&reading());
        shows(
            &frame,
            &[
                t("upd-up-to-date"),
                t("upd-check"),
                t_args(
                    "upd-image-built",
                    &[("image", "Tilo 2026.06".into()), ("date", "2026-06-02".into())],
                ),
                t_args(
                    "upd-previous-image",
                    &[(
                        "image",
                        t_args(
                            "upd-image-built",
                            &[("image", "Tilo 2026.05".into()), ("date", "2026-06-02".into())],
                        )
                        .into(),
                    )],
                ),
            ],
            &[t("upd-restart-update"), t("upd-finish-now"), t("upd-checking")],
        );
    }

    phase the_firmware_line_never_shows_what_the_firmware_tool_said;
    {
        // The pane's firmware reader fails with fwupd's own words; the page says firmware cannot
        // be checked here and offers to ask again.
        let frame = page_reading(&reading());
        shows(
            &frame,
            &[t("upd-fw-query-failed"), t("upd-try-again")],
            &["fwupd is unavailable in this test".into()],
        );
    }

    phase checking_says_so_and_offers_nothing_else;
    {
        let frame = page_reading(&Update {
            updating: true,
            pending: vec!["update-now".into()],
            ..reading()
        });
        shows(&frame, &[t("upd-checking")], &[t("upd-check"), t("upd-up-to-date")]);
    }

    phase only_the_desktop_half_offers_a_desktop_restart_that_keeps_the_apps;
    {
        let frame = page_reading(&Update {
            desktop_pending: true,
            restart_needed: false,
            ..staged()
        });
        shows(
            &frame,
            &[t("upd-live-ready"), t("upd-finishes-now"), t("upd-finish-now")],
            &[t("upd-restart-update")],
        );
    }

    phase a_restart_of_the_computer_names_the_version_it_finishes;
    {
        let frame = page_reading(&staged());
        shows(
            &frame,
            &[
                t_args("upd-ready", &[("version", "Tilo 2026.07".into())]),
                t("upd-restart-apply"),
                t("upd-restart-update"),
            ],
            &[t("upd-finish-now"), t("upd-up-to-date")],
        );
    }

    phase a_live_update_that_failed_says_what_to_do_and_not_why;
    {
        let frame = page_reading(&Update {
            failed: Some("tilo-session exited with status 1".into()),
            ..staged()
        });
        shows(
            &frame,
            &[t("upd-live-failed-title"), t("upd-live-failed"), t("upd-restart-update")],
            &[t("upd-finish-now"), "exited with status 1".into()],
        );
    }

    phase the_desktop_restarting_says_the_apps_stay_open;
    {
        let frame = page_reading(&Update {
            desktop_pending: true,
            restart_needed: false,
            pending: vec!["restart-desktop".into()],
            ..staged()
        });
        shows(&frame, &[t("upd-finishing")], &[t("upd-finish-now")]);
    }

    phase an_update_applied_in_full_is_done;
    {
        let frame = page_reading(&Update {
            applied: true,
            restart_needed: false,
            ..staged()
        });
        let built = |version: &str| {
            t_args(
                "upd-image-built",
                &[("image", version.into()), ("date", "2026-06-02".into())],
            )
        };
        // The update runs now, though the machine started the version before it.
        shows(
            &frame,
            &[t("upd-up-to-date"), t("upd-done"), t("upd-check"), built("Tilo 2026.07")],
            &[t("upd-restart-update"), t("upd-finish-now"), built("Tilo 2026.06")],
        );
    }

    phase a_member_who_may_not_see_the_deployments_is_told_so;
    {
        let frame = page_reading(&Update {
            booted: None,
            rollback: None,
            ..Update::default()
        });
        shows(
            &frame,
            &[t("upd-admins-only"), t("upd-up-to-date")],
            &[t("upd-no-previous")],
        );
    }

    phase a_session_without_the_update_says_so_and_claims_nothing;
    {
        let frame = page_of(|sections| {
            let feed = Arc::new(HeldReading::default());
            feed.fail("no service publishes system-update");
            pane_reading_in(sections, feed)
        });
        shows(
            &frame,
            &[t("upd-unreachable")],
            &[t("upd-up-to-date"), t("upd-no-previous"), "no service publishes".into()],
        );
    }
}

tilo_test_env::scenario! {
    fn the_pane_follows_the_session_and_asks_it_for_its_actions;

    phase each_reading_moves_the_page_to_its_state;
    {
        let (mut pane, feed) =
            pane_reading(&reading(), &Arc::new(SessionHost::default()), Arc::new(UnavailableControl));
        assert_eq!(pane.update_state(), UpdateState::UpToDate);
        for (update, state) in [
            (
                Update { desktop_pending: true, restart_needed: false, ..staged() },
                UpdateState::DesktopRestart,
            ),
            (
                Update { failed: Some("exit 1".into()), ..staged() },
                UpdateState::Failed,
            ),
            (staged(), UpdateState::Restart),
            (Update { applied: true, restart_needed: false, ..staged() }, UpdateState::Done),
        ] {
            feed.set(&update);
            assert!(pane.poll(), "a reading redraws the page");
            assert_eq!(pane.update_state(), state, "{update:?}");
        }
        assert!(!pane.poll(), "nothing new, nothing to draw");
    }

    phase a_check_is_the_sessions_and_stamps_when_it_ends_well;
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let host = Arc::new(SessionHost::default());
        let (mut pane, feed) = pane_reading(&reading(), &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::CheckForUpdates, None);
        assert_eq!(pane.update_state(), UpdateState::Checking, "said at once");
        // A second press while the first is unanswered asks nothing more.
        pane.run(UpdatesEffect::CheckForUpdates, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());
        assert_eq!(host.asked(), [("system-update".into(), "update-now".into(), false)]);
        assert_eq!(pane.prefs().last_checked, "Never", "taken is not checked");

        feed.set(&Update { updating: true, pending: vec!["update-now".into()], ..reading() });
        pane.poll();
        assert_eq!(pane.update_state(), UpdateState::Checking);
        feed.set(&staged());
        pane.poll();
        assert_eq!(pane.update_state(), UpdateState::Restart);
        assert_eq!(pane.prefs().last_checked, "CHECKED-NOW");
        assert!(pane.notice().is_none());

        // A check that did not finish is said plainly, without the unit's words, and is no check.
        let (mut pane, feed) = pane_reading(&reading(), &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::CheckForUpdates, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());
        feed.set(&Update { updating: true, pending: vec!["update-now".into()], ..reading() });
        pane.poll();
        feed.set(&Update {
            problem: Some("tilo-deployment-update.service failed".into()),
            ..reading()
        });
        pane.poll();
        assert_eq!(pane.notice(), Some(t("upd-notice-check").as_str()));
        assert_eq!(pane.prefs().last_checked, "Never");
    }

    phase the_restarts_ask_the_session_with_the_persons_confirmation;
    {
        let host = Arc::new(SessionHost::default());
        let desktop = Update { desktop_pending: true, restart_needed: false, ..staged() };
        let (mut pane, _) = pane_reading(&desktop, &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::RestartDesktop, None);
        assert_eq!(pane.update_state(), UpdateState::Finishing, "said at once");
        settle(&mut pane, |pane| !pane.asked.is_empty());

        let (mut pane, _) = pane_reading(&staged(), &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::RestartToUpdate, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());

        // Firmware that waits for a restart takes the computer's own.
        let (mut pane, _) = pane_reading(&reading(), &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::RestartToUpdate, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());

        assert_eq!(
            host.asked(),
            [
                ("system-update".into(), "restart-desktop".into(), false),
                ("system-update".into(), "restart-to-update".into(), true),
                ("power".into(), "restart".into(), true),
            ]
        );
        assert!(pane.notice().is_none());
    }

    phase a_refusal_says_what_to_do_and_keeps_the_sessions_words_to_the_log;
    {
        let host = Arc::new(SessionHost::default());
        host.refuse("No part of the update waits for the desktop");
        let desktop = Update { desktop_pending: true, restart_needed: false, ..staged() };
        let (mut pane, _) = pane_reading(&desktop, &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::RestartDesktop, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());
        assert_eq!(pane.notice(), Some(t("upd-notice-desktop-failed").as_str()));
        assert_eq!(pane.update_state(), UpdateState::DesktopRestart, "it can be asked again");
    }

    phase a_desktop_restart_that_did_not_finish_is_said_once_it_ends;
    {
        let host = Arc::new(SessionHost::default());
        let desktop = Update { desktop_pending: true, restart_needed: false, ..staged() };
        let (mut pane, feed) = pane_reading(&desktop, &host, Arc::new(UnavailableControl));
        pane.run(UpdatesEffect::RestartDesktop, None);
        settle(&mut pane, |pane| !pane.asked.is_empty());
        feed.set(&Update { pending: vec!["restart-desktop".into()], ..desktop.clone() });
        pane.poll();
        assert!(pane.notice().is_none(), "it still runs");
        feed.set(&Update { problem: Some("tilo-live-update-session.service failed".into()), ..desktop });
        pane.poll();
        assert_eq!(pane.notice(), Some(t("upd-notice-desktop-failed").as_str()));
    }
}

tilo_test_env::scenario! {
    fn updates_are_applied_and_scheduled_through_their_controls;

    phase firmware_refreshes_run_promptly_and_say_plainly_when_they_fail;
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(None));
        let release = control.hold_refreshes();
        let mut pane = pane_with_control(Arc::clone(&control));
        pane.poll();
        pane.run(UpdatesEffect::RefreshFirmware, None);
        pane.poll();
        assert!(pane.firmware_refresh_pending);
        release.send(()).unwrap();
        settle(&mut pane, |pane| pane.firmware_refresh_pending);
        assert!(
            !control.hold_timed_out(),
            "sync blocked on the firmware refresh"
        );
        assert_eq!(pane.firmware_updates, Ok(Vec::new()));

        control.set_refresh_fails(true);
        pane.run(UpdatesEffect::RefreshFirmware, None);
        pane.poll();
        assert!(pane.firmware_refresh_pending);
        release.send(()).unwrap();
        settle(&mut pane, |pane| pane.firmware_refresh_pending);
        assert!(pane.firmware_updates.is_err());
        assert_eq!(
            pane.notice(),
            Some(t("upd-notice-fw-refresh").as_str()),
            "fwupd's words are for the log"
        );
    }

    phase rollback_is_confirmed_and_reported;
    {
        // Rolling back takes two clicks, and the first one only shows what a
        // rollback does not undo.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true)));
            let mut pane = pane_with_control(control.clone());
            pane.poll();
            assert_eq!(
                pane.rollback_target(),
                Ok(Some(t_args(
                    "upd-image-built",
                    &[("image", "Tilo 2026.05".into()), ("date", "2026-06-02".into())],
                ))),
                "what the session reads it can go back to"
            );

            pane.run(UpdatesEffect::ReviewRollback, None);
            pane.poll();
            assert_eq!(
                *control.rollbacks.lock().unwrap(),
                0,
                "the first click must only ask; a one-click rollback is a rollback \
             by accident"
            );

            let shown = pane.rollback_card_lines().join("\n");
            for phrase in ["do not revert", "next app update"] {
                assert!(
                    shown.contains(phrase),
                    "the confirmation must say what a rollback does not undo \
                 ({phrase} missing) — shown was:\n{shown}"
                );
            }

            pane.run(UpdatesEffect::RollBack, None);
            pane.poll();
            settle(&mut pane, UpdatesModel::working);
            assert_eq!(*control.rollbacks.lock().unwrap(), 1);
            let notice = pane.notice().expect("the result has to be shown");
            assert!(
                notice.contains("Restart"),
                "the person has to be told a restart is what takes it, got: {notice}"
            );
        }

        // A failed rollback says so where the person is looking.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true)).failing_rollback());
            let mut pane = pane_with_control(control);
            pane.poll();
            pane.run(UpdatesEffect::RollBack, None);
            pane.poll();
            settle(&mut pane, UpdatesModel::working);
            let notice = pane.notice().expect("a failed rollback must be visible");
            assert!(
                notice.contains("the boot loader would not take it"),
                "the reason has to survive to the screen, got: {notice}"
            );
        }

        // The help text must never promise a capability the pane does not offer
        // — assert on what the pane renders, not on a comment.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane = pane_with_control(Arc::new(FakeControl::new(Some(true))));
            let help = pane.help_lines().join("\n");
            assert!(
                !help.contains("you can roll back to the previous image at boot"),
                "the old sentence described a feature that did not exist; if it is \
             back, so is the lie. Help text was:\n{help}"
            );
            assert_eq!(
                pane.rollback_card_lines()[0],
                rollback_card_title(),
                "the rollback control has to be on the pane, since the pane talks \
             about rolling back"
            );
            assert!(
                help.contains("three times"),
                "the automatic half is the part a person cannot discover by \
             looking, so the pane has to state it. Help text was:\n{help}"
            );
        }

        // A machine with nothing to go back to says so, rather than offering a
        // button that would fail.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let (pane, _) = pane_reading(
                &Update { rollback: None, ..reading() },
                &Arc::new(SessionHost::default()),
                Arc::new(FakeControl::new(Some(true))),
            );
            let shown = pane.rollback_card_lines().join("\n");
            assert!(
                shown.contains("no previous image on this machine yet"),
                "the absence has to be stated; an absent card reads as an absent \
             feature. Shown was:\n{shown}"
            );
        }
    }

    phase automatic_update_switch_follows_the_timer;
    {
        // The switch shows the timer, not a stored preference, even when the two
        // disagree.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane = UpdatesModel::with_parts(
                test_cx(),
                UpdatePrefs {
                    auto: false,
                    ..UpdatePrefs::default()
                },
                HeldReading::of(&reading()),
                Box::new(NoActions::default()),
                Arc::new(|| "now".to_string()),
                Arc::new(FakeControl::new(Some(true))),
            );
            assert!(
                pane.prefs().auto,
                "the machine's timer wins over a stale preference; showing the \
             preference is how a switch comes to lie about the system"
            );
            assert_eq!(pane.timer_enabled(), Some(true));
        }

        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true)));
            let mut pane = pane_with_control(control.clone());
            pane.poll();

            pane.run(UpdatesEffect::SetAutomaticUpdates { on: false }, None);
            pane.poll();
            settle(&mut pane, UpdatesModel::working);

            assert_eq!(
                *control.set_calls.lock().unwrap(),
                vec![false],
                "the switch must reach the update timer exactly once"
            );
            assert_eq!(
                control.timer_enabled(),
                Some(false),
                "and the timer must actually be off afterwards"
            );
            assert_eq!(pane.timer_enabled(), Some(false));
            assert!(
                pane.notice().is_none(),
                "nothing went wrong, so say nothing"
            );
        }

        // When the system refuses, the switch goes back and the pane says so on
        // screen. A warning in the journal is, for a settings switch, the same as
        // failing silently.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let control = Arc::new(FakeControl::new(Some(true)).failing_set());
            let mut pane = pane_with_control(control.clone());
            pane.poll();

            pane.run(UpdatesEffect::SetAutomaticUpdates { on: false }, None);
            pane.poll();
            settle(&mut pane, UpdatesModel::working);

            assert_eq!(
                control.timer_enabled(),
                Some(true),
                "the timer must be untouched when the change was refused"
            );
            assert!(
                pane.prefs().auto,
                "the switch itself must snap back, or it shows a state the machine \
             is not in"
            );
            let notice = pane
                .notice()
                .expect("a refused change has to appear in the pane, not only the journal");
            assert!(
                notice.contains("polkit said no"),
                "the message must carry the reason, got: {notice}"
            );
        }

        // A machine that cannot be asked gets no switch at all. Rendering "off"
        // for a timer that could not be read is the same class of lie.
        {
            let _cfg = crate::prefs::test_env::ConfigGuard::new();
            let pane = pane_with_control(Arc::new(FakeControl::new(None)));
            assert_eq!(pane.timer_enabled(), None);
            assert_eq!(UnavailableControl.timer_enabled(), None);
            assert!(UnavailableControl.set_timer(true).is_err());
            assert!(UnavailableControl.roll_back().is_err());
        }
    }
}

/// Drive the host's polling until `pending` clears. The bound is a hang
/// guard for a worker that never reports, not a performance budget.
fn settle(pane: &mut UpdatesModel, pending: impl Fn(&UpdatesModel) -> bool) {
    let deadline = ui::time::now() + HOLD_TIMEOUT;
    loop {
        pane.poll();
        if !pending(pane) {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "background worker never reported"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct FakeControl {
    timer: Mutex<Option<bool>>,
    set_calls: Mutex<Vec<bool>>,
    rollbacks: Mutex<u32>,
    set_fails: bool,
    rollback_fails: bool,
    refresh_hold: Mutex<Option<channel::Receiver<()>>>,
    refresh_fails: Mutex<bool>,
    hold_timed_out: std::sync::atomic::AtomicBool,
}

const HOLD_TIMEOUT: Duration = Duration::from_secs(30);

impl FakeControl {
    fn new(timer: Option<bool>) -> Self {
        Self {
            timer: Mutex::new(timer),
            set_calls: Mutex::new(Vec::new()),
            rollbacks: Mutex::new(0),
            set_fails: false,
            rollback_fails: false,
            refresh_hold: Mutex::new(None),
            refresh_fails: Mutex::new(false),
            hold_timed_out: std::sync::atomic::AtomicBool::new(false),
        }
    }
    fn failing_set(mut self) -> Self {
        self.set_fails = true;
        self
    }
    fn failing_rollback(mut self) -> Self {
        self.rollback_fails = true;
        self
    }
    /// Hold every firmware refresh until the returned sender releases it.
    fn hold_refreshes(&self) -> channel::Sender<()> {
        let (release, hold) = channel::channel();
        *self.refresh_hold.lock().unwrap() = Some(hold);
        release
    }
    fn wait_at(&self, hold: &Mutex<Option<channel::Receiver<()>>>) {
        if let Some(hold) = hold.lock().unwrap().as_ref()
            && hold.recv_timeout(HOLD_TIMEOUT).is_err()
        {
            self.hold_timed_out
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn hold_timed_out(&self) -> bool {
        self.hold_timed_out
            .load(std::sync::atomic::Ordering::SeqCst)
    }
    fn set_refresh_fails(&self, fails: bool) {
        *self.refresh_fails.lock().unwrap() = fails;
    }
}

impl UpdateControl for FakeControl {
    fn timer_enabled(&self) -> Option<bool> {
        crate::device_read::off_ui_thread();
        *self.timer.lock().unwrap()
    }
    fn set_timer(&self, on: bool) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        self.set_calls.lock().unwrap().push(on);
        if self.set_fails {
            return Err("polkit said no".into());
        }
        *self.timer.lock().unwrap() = Some(on);
        Ok(())
    }
    fn roll_back(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        *self.rollbacks.lock().unwrap() += 1;
        if self.rollback_fails {
            return Err("the boot loader would not take it".into());
        }
        Ok(())
    }

    fn refresh_firmware(
        &self,
    ) -> Result<
        (
            Vec<os_actions::FirmwareUpdate>,
            Vec<os_actions::FirmwareHistoryEntry>,
        ),
        String,
    > {
        crate::device_read::off_ui_thread();
        self.wait_at(&self.refresh_hold);
        if *self.refresh_fails.lock().unwrap() {
            Err("firmware refresh failed".into())
        } else {
            Ok((Vec::new(), Vec::new()))
        }
    }
}

fn pane_with_control(control: Arc<FakeControl>) -> UpdatesModel {
    pane_reading(&reading(), &Arc::new(SessionHost::default()), control).0
}

#[test]
fn a_check_and_the_timer_are_kept_in_the_update_preferences() {
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let feed = HeldReading::of(&reading());
    let model = app
        .with_widget::<crate::window::Inspected, _>(|settings| {
            let shared = std::rc::Rc::clone(crate::panes::updates::area(settings).unwrap().model());
            *shared.borrow_mut() = pane_reading_in(settings.sections().clone(), Arc::clone(&feed));
            shared
        })
        .expect("the Settings window is mounted");
    app.effect(&SettingsEffect::from(UpdatesEffect::CheckForUpdates))
        .expect("the session takes the check");
    feed.set(&Update {
        updating: true,
        pending: vec!["update-now".into()],
        ..reading()
    });
    model.borrow_mut().poll();
    feed.set(&reading());
    model.borrow_mut().poll();
    app.effect(&SettingsEffect::from(UpdatesEffect::SetAutomaticUpdates {
        on: false,
    }))
    .expect("the timer turns off");
    app.settle();
    let kept = app.settings::<UpdatePrefs>().get();
    assert_eq!(kept.last_checked, "CHECKED-NOW");
    assert!(!kept.auto, "the timer the machine reports is kept");
}

#[test]
fn effects_reach_the_model_and_report_how_they_end() {
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let refused = app
        .effect(&SettingsEffect::from(UpdatesEffect::CheckForUpdates))
        .unwrap_err();
    assert!(
        format!("{refused:?}").contains(&t("upd-notice-check")),
        "a capture asks the session nothing: {refused:?}"
    );
    let refused = app
        .effect_confirmed(&SettingsEffect::from(UpdatesEffect::RollBack))
        .unwrap_err();
    assert!(
        format!("{refused:?}").contains("Rolling back failed"),
        "{refused:?}"
    );
    app.effect(&SettingsEffect::from(
        crate::effects::NavigationEffect::OpenPane {
            pane: "updates".into(),
        },
    ))
    .unwrap();
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains("Tilo 2026.07"), "{frame}");
}

/// A control whose every call waits for its hold to open: a device that
/// takes as long as it likes. The hold gives up after ten seconds so a
/// broken test ends.
struct HeldControl {
    hold: Arc<crate::stall::Stall>,
    timer: Mutex<Option<bool>>,
    set_calls: Mutex<Vec<bool>>,
    rollbacks: Mutex<u32>,
}

impl HeldControl {
    fn pass(&self) {
        crate::device_read::off_ui_thread();
        self.hold.wait();
    }
}

impl UpdateControl for HeldControl {
    fn timer_enabled(&self) -> Option<bool> {
        self.pass();
        *self.timer.lock().unwrap()
    }
    fn set_timer(&self, on: bool) -> Result<(), String> {
        self.pass();
        self.set_calls.lock().unwrap().push(on);
        *self.timer.lock().unwrap() = Some(on);
        Ok(())
    }
    fn roll_back(&self) -> Result<(), String> {
        self.pass();
        *self.rollbacks.lock().unwrap() += 1;
        Ok(())
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the stalled device's whole story, read and change, reads best as one test"
)]
fn a_stalled_update_control_never_holds_the_window() {
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().unwrap();
    let hold = crate::stall::Stall::new();
    let control = Arc::new(HeldControl {
        hold: Arc::clone(&hold),
        timer: Mutex::new(Some(true)),
        set_calls: Mutex::new(Vec::new()),
        rollbacks: Mutex::new(0),
    });
    let readers = DeviceReaders {
        details: Arc::new(UpdateDetails::default),
        control: Arc::clone(&control) as Arc<dyn UpdateControl>,
        firmware: Arc::new(|| -> FirmwareReading {
            crate::device_read::off_ui_thread();
            (Ok(Vec::new()), Ok(Vec::new()))
        }),
        codec_status: Arc::new(CodecPackStatus::default),
        windows_support: Arc::new(|| WindowsReadiness::NotOffered),
    };
    let model = app
        .with_widget::<crate::window::Inspected, _>(move |settings| {
            let shared = std::rc::Rc::clone(crate::panes::updates::area(settings).unwrap().model());
            let mut model =
                UpdatesModel::reading(settings.storage(), settings.sections().clone(), readers);
            model.feed = Some(HeldReading::of(&reading()));
            *shared.borrow_mut() = model;
            shared
        })
        .expect("the Settings window is mounted");

    let clicked = std::time::Instant::now();
    crate::effects::show_pane(&mut app, crate::categories::Category::Updates);
    app.harness()
        .poll_frame()
        .expect("the window draws its next frame");
    let answered = clicked.elapsed();
    assert!(
        answered < Duration::from_secs(1),
        "opening the pane waited {answered:?} on the update tools"
    );
    let frame = app.harness().inspection_json().unwrap();
    assert!(frame.contains(&t("upd-reading")), "{frame}");
    assert!(!frame.contains(&t("upd-up-to-date")), "{frame}");

    // A request while the device is still being read waits for the read.
    let clicked = std::time::Instant::now();
    model
        .borrow_mut()
        .run(UpdatesEffect::SetAutomaticUpdates { on: false }, None);
    app.harness()
        .poll_frame()
        .expect("the window draws its next frame");
    let answered = clicked.elapsed();
    assert!(
        answered < Duration::from_secs(1),
        "the switch waited {answered:?} on the update timer"
    );
    assert!(control.set_calls.lock().unwrap().is_empty());

    hold.release();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(30), |harness| {
                model.borrow_mut().poll();
                model.borrow().timer_enabled() == Some(false)
                    && harness
                        .inspection_json()
                        .is_ok_and(|frame| frame.contains(&t("upd-up-to-date")))
            }),
        "the read lands, then the waiting request turns the timer off"
    );
    assert_eq!(*control.set_calls.lock().unwrap(), vec![false]);

    hold.hold();
    let clicked = std::time::Instant::now();
    model.borrow_mut().run(UpdatesEffect::RollBack, None);
    app.harness()
        .poll_frame()
        .expect("the window draws its next frame");
    let answered = clicked.elapsed();
    assert!(
        answered < Duration::from_secs(1),
        "the rollback button waited {answered:?} on the rollback"
    );
    assert_eq!(model.borrow().notice(), None);

    hold.release();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(30), |_| model
                .borrow()
                .notice()
                .is_some_and(|notice| notice.contains("Restart"))),
        "the rollback's outcome is shown"
    );
    assert_eq!(*control.rollbacks.lock().unwrap(), 1);
}

/// What the window publishes for `id`: search and agents read the offer from this.
fn published(
    app: &mut ui::framework::testing::TestApp<crate::SettingsShell>,
    id: &str,
) -> ui::tilo_actions::Availability {
    app.actions()
        .expect("the actions are published")
        .into_iter()
        .find(|action| action.address.action == id)
        .unwrap_or_else(|| panic!("`{id}` is published"))
        .contract
        .availability
}

/// The install and remove offers a window publishes with Windows support `readiness`.
fn windows_offers(
    readiness: WindowsReadiness,
) -> (
    ui::tilo_actions::Availability,
    ui::tilo_actions::Availability,
) {
    crate::area::replace_in_next_window(move |context| {
        let mut area = UpdatesArea::new(context);
        area.set_windows_support(readiness);
        area
    });
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    app.settle();
    (
        published(&mut app, "install-windows-support"),
        published(&mut app, "remove-windows-support"),
    )
}

/// The offer search shows is the one the device can act on: installing once it is installed is
/// not offered, and removing what is not there is not either.
#[test]
fn the_windows_support_offer_follows_the_installed_state() {
    use ui::tilo_actions::Availability;

    let installed = windows_offers(WindowsReadiness::Ready);
    assert_eq!(
        installed.0,
        Availability::Unavailable {
            code: "windows.support".into(),
            reason: "Windows support is installed.".into(),
        }
    );
    assert_eq!(installed.1, Availability::Available);

    let missing = windows_offers(needs_install());
    assert_eq!(missing.0, Availability::Available);
    assert!(
        matches!(&missing.1, Availability::Unavailable { code, reason }
            if code == "windows.support" && reason.starts_with("Windows support is not installed.")),
        "{:?}",
        missing.1
    );
}

/// Every state Settings can read hides exactly the offer it cannot carry out.
#[test]
fn each_windows_support_state_hides_what_it_cannot_do() {
    let cases = [
        (WindowsReadiness::Ready, vec!["install-windows-support"]),
        (needs_install(), vec!["remove-windows-support"]),
        (
            WindowsReadiness::Installing(None),
            vec!["install-windows-support", "remove-windows-support"],
        ),
        (
            WindowsReadiness::WaitingForNetwork,
            vec!["install-windows-support"],
        ),
        (
            WindowsReadiness::Failed("the network went away".into(), disclosure()),
            Vec::new(),
        ),
        (
            WindowsReadiness::NotOffered,
            vec!["install-windows-support", "remove-windows-support"],
        ),
    ];
    for (readiness, hidden) in cases {
        let statuses = super::windows_card::support_statuses(&readiness);
        let ids: Vec<&str> = statuses
            .iter()
            .map(ui::framework::EffectStatus::id)
            .collect();
        assert_eq!(ids, hidden, "{readiness:?}");
        for status in &statuses {
            assert_eq!(
                status.availability(),
                Some(&ui::tilo_actions::Availability::Unavailable {
                    code: "windows.support".into(),
                    reason: readiness.message(),
                }),
                "{readiness:?}"
            );
        }
    }
}

/// A person who asks for Windows support from somewhere else (search, an
/// agent's action) lands on the card that shows the download, so its end is
/// where they were sent rather than a change they cannot see.
#[test]
fn asking_for_windows_support_opens_its_card() {
    crate::area::replace_in_next_window(|context| {
        let mut area = UpdatesArea::new(context);
        area.set_windows_support(needs_install());
        area.model().borrow_mut().windows_support_runner = Arc::new(|_| Ok(()));
        area
    });
    let mut app = ui::framework::testing::TestApp::<crate::SettingsShell>::new().unwrap();
    app.settle();
    app.effect(&SettingsEffect::from(UpdatesEffect::InstallWindowsSupport))
        .expect("the install is accepted");
    app.settle();
    let shown = app
        .with_widget::<crate::window::Inspected, _>(|window| window.selected())
        .expect("the Settings window is mounted");
    assert_eq!(shown, crate::categories::Category::Updates);
}
