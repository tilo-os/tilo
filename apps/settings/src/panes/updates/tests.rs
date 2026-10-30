use super::*;
use std::sync::Arc;
use std::sync::Mutex;

/// A model's sections, kept nowhere.
fn test_cx() -> crate::sections::Sections {
    crate::sections::Sections::default()
}

#[derive(Default)]
struct RecordingRebooter {
    calls: Mutex<u32>,
}
impl Rebooter for RecordingRebooter {
    fn reboot(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        *self.calls.lock().unwrap() += 1;
        Ok(())
    }
}

fn pane_with(staged: Option<&str>, rebooter: Arc<RecordingRebooter>) -> UpdatesModel {
    let status = UpdateStatus {
        current_version: "Tilo 2026.06".into(),
        staged_version: staged.map(std::string::ToString::to_string),
        staged_error: None,
    };
    let requeried = UpdateStatus {
        current_version: "Tilo 2026.06".into(),
        staged_version: Some("Tilo 2026.07".into()),
        staged_error: None,
    };
    UpdatesModel::with_parts(
        test_cx(),
        status,
        UpdatePrefs::default(),
        Arc::new(move || requeried.clone()),
        Arc::new(|| "CHECKED-NOW".to_string()),
        rebooter,
    )
}

/// A pane whose codec runner rewrites a fake status file the way the
/// helper does and returns `outcome`; `status` is what the watcher reads.
fn codec_pane(
    status: Arc<Mutex<String>>,
    runs: Arc<Mutex<Vec<CodecAction>>>,
    outcome: Result<(), String>,
) -> UpdatesModel {
    let reader = Arc::clone(&status);
    pane_with(None, Arc::new(RecordingRebooter::default())).with_codec_pack(
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

/// Press a codec control, then wait for the unit to return (no polling).
fn press_and_finish(pane: &mut UpdatesModel, effect: UpdatesEffect) {
    pane.poll();
    pane.run(effect, None);
    pane.poll();
    pane.wait_for_codec_operation();
    pane.poll();
}

#[test]
fn codec_card_is_one_short_line_and_one_button() {
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

#[test]
fn every_helper_state_has_its_status_and_controls() {
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

#[test]
fn a_running_install_shows_the_helpers_progress() {
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

#[test]
fn the_watcher_updates_the_card_while_the_helper_works() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let status = shared_status(r#"{"state":"waiting-for-network","desired":true}"#);
    let mut pane = codec_pane(Arc::clone(&status), runs, Ok(()));
    pane.poll();
    // The device came online and the helper installed in the background.
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

#[test]
fn install_runs_the_unit_and_shows_the_waiting_mesa_part() {
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

#[test]
fn a_request_the_helper_has_not_picked_up_reads_as_starting() {
    let (release, gate) = channel::channel::<()>();
    let gate = Mutex::new(gate);
    let mut pane = pane_with(None, Arc::new(RecordingRebooter::default())).with_codec_pack(
        Arc::new(CodecPackStatus::default),
        Arc::new(move |_| {
            let _ = gate.lock().unwrap().recv();
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

#[test]
fn failed_install_surfaces_the_helper_error_and_offers_retry() {
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

#[test]
fn checking_again_for_the_mesa_build_runs_a_refresh() {
    let runs = Arc::new(Mutex::new(Vec::new()));
    let status = shared_status(
        r#"{"state":"installed","parts":{"tilo-codecs":"enabled","tilo-codecs-mesa":"waiting"},"desired":true}"#,
    );
    let mut pane = codec_pane(status, Arc::clone(&runs), Ok(()));
    press_and_finish(&mut pane, UpdatesEffect::RefreshCodecs);
    assert_eq!(*runs.lock().unwrap(), vec![CodecAction::Refresh]);
}

#[test]
fn remove_returns_to_not_installed() {
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

/// A pane whose Windows-support runner returns `outcome`; `readiness` is
/// what the card reads next.
fn windows_pane(
    readiness: Arc<Mutex<WindowsReadiness>>,
    runs: Arc<Mutex<Vec<WindowsSupportAction>>>,
    outcome: Result<(), String>,
    after: WindowsReadiness,
) -> UpdatesModel {
    let reader = Arc::clone(&readiness);
    let mut pane = pane_with(None, Arc::new(RecordingRebooter::default()));
    pane.windows_support = (reader.lock().unwrap()).clone();
    pane.windows_support_provider = Arc::new(move || (reader.lock().unwrap()).clone());
    pane.windows_support_runner = Arc::new(move |action| {
        runs.lock().unwrap().push(action);
        *readiness.lock().unwrap() = after.clone();
        outcome.clone()
    });
    pane
}

/// The sizes this image's own catalogue entry declares.
fn disclosure() -> crate::windows_compat::SizeDisclosure {
    crate::windows_compat::SizeDisclosure::parse(include_str!(
        "../../../../../os/usr/share/tilo/system-extensions/sysext.d/windows-compat.json"
    ))
    .expect("the image's catalogue entry declares the size")
}

fn needs_install() -> WindowsReadiness {
    WindowsReadiness::NeedsInstall(disclosure())
}

/// Press a Windows-support control, then wait for the unit to return.
fn press_windows(pane: &mut UpdatesModel, effect: UpdatesEffect) {
    pane.poll();
    pane.run(effect, None);
    while pane.windows_events.is_some() {
        pane.drain_windows_events();
        std::thread::yield_now();
    }
    pane.poll();
}

#[test]
fn windows_card_discloses_the_download_before_it_installs() {
    let pane = windows_pane(
        Arc::new(Mutex::new(needs_install())),
        Arc::new(Mutex::new(Vec::new())),
        Ok(()),
        WindowsReadiness::Ready,
    );
    let lines = pane.windows_support_card_lines();
    assert_eq!(lines.first().map(String::as_str), Some("Run Windows apps"));
    // The figure is the catalogue's, in the person's words, on the line
    // that also says nothing is there yet.
    assert!(
        lines.iter().any(|line| line
            .starts_with("Status: Not installed. Windows support is a separate download")
            && line.contains("about 1.9 GB")
            && line.contains("about 3.1 GB")),
        "{lines:?}"
    );
    assert_eq!(lines.last().map(String::as_str), Some("Install"));
}

#[test]
fn installing_windows_support_runs_the_extension_and_shows_its_progress() {
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

#[test]
fn a_running_windows_install_shows_the_helpers_step() {
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

#[test]
fn a_failed_windows_install_surfaces_the_error_and_offers_retry() {
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

#[test]
fn removing_windows_support_returns_to_the_download_offer() {
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

#[test]
fn a_cancelled_install_offers_only_starting_it_again() {
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

/// os-release, bootc staged/rollback, release notes and journal history parsing.
#[test]
fn update_status_parsers_accept_only_trusted_shapes() {
    // Case `parses_pretty_name`.
    {
        assert_eq!(
            parse_pretty_name("PRETTY_NAME=\"Tilo 42\"\n").as_deref(),
            Some("Tilo 42")
        );
    }

    // Case `parse_bootc_staged_detects_and_ignores`.
    {
        let staged = r#"{"status":{"staged":{"version":"Tilo 2026.07"},"booted":{"version":"Tilo 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(staged).as_deref(), Some("Tilo 2026.07"));

        let none = r#"{"status":{"staged":null,"booted":{"version":"Tilo 2026.06"}}}"#;
        assert_eq!(parse_bootc_staged(none), None);

        let image =
            r#"{"status":{"staged":{"image":{"image":{"image":"quay.io/tilo/tilo:latest"}}}}}"#;
        assert_eq!(
            parse_bootc_staged(image).as_deref(),
            Some("quay.io/tilo/tilo:latest")
        );
    }

    // Case `parse_bootc_rollback_detects_and_ignores`.
    {
        let has = r#"{"status":{"rollback":{"image":{"image":{"image":"tilo:gen41"}}}}}"#;
        assert_eq!(parse_bootc_rollback(has).as_deref(), Some("tilo:gen41"));
        let dated = r#"{"status":{"rollback":{"image":{"image":{"image":"tilo:stable"},
                "version":"2026.10.18","timestamp":"2026-10-18T04:12:00Z"}}}}"#;
        assert_eq!(
            parse_bootc_rollback(dated).as_deref(),
            Some("2026.10.18, built 2026-10-18")
        );
        let none = r#"{"status":{"rollback":null}}"#;
        assert_eq!(parse_bootc_rollback(none), None);
        let bare = r#"{"status":{"rollback":{}}}"#;
        assert_eq!(
            parse_bootc_rollback(bare).as_deref(),
            Some("the previous image"),
            "a rollback deployment bootc describes in a shape we do not know \
             still exists, and hiding it would hide the way back"
        );
    }

    // Case `release_notes_only_accept_https_image_metadata`.
    {
        let json = r#"{"status":{"staged":{"image":{"annotations":{"org.opencontainers.image.documentation":"https://updates.example.test/releases/42"}}}}}"#;
        assert_eq!(
            parse_bootc_release_notes(json).as_deref(),
            Some("https://updates.example.test/releases/42")
        );
        let unsafe_url = r#"{"releaseNotes":"file:///etc/shadow"}"#;
        assert_eq!(parse_bootc_release_notes(unsafe_url), None);
    }

    // Case `journal_history_is_bounded_and_control_free`.
    {
        let history = parse_update_history("2026-01-01 update complete\nline\u{0007} two\n");
        assert_eq!(history, ["2026-01-01 update complete", "line two"]);
    }
}

/// Checks and firmware refresh return promptly, stamp only on success, dedupe, and ignore replicated latches.
#[test]
fn update_checks_run_promptly_and_stamp_only_on_completion() {
    // Case `check_requeries_and_stamps_last_checked`.
    // Check for updates re-queries the system and stamps a real
    // last-checked time — proving the button acts, not no-ops.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(None, reb);
        pane.poll();
        assert_eq!(pane.prefs().last_checked, "Never");
        assert_eq!(pane.status().staged_version, None);
        // Press Check.
        pane.run(UpdatesEffect::CheckForUpdates, None);
        pane.poll();
        settle(&mut pane, |pane| pane.check_pending);
        assert_eq!(
            pane.prefs().last_checked,
            "CHECKED-NOW",
            "timestamp updated"
        );
        assert_eq!(
            pane.status().staged_version.as_deref(),
            Some("Tilo 2026.07"),
            "the re-query result landed"
        );
    }

    // Case `blocking_update_check_returns_promptly_and_stamps_only_on_completion`.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(None, None));
        let release = control.gate_checks();
        let mut pane = pane_with_control(Arc::clone(&control));
        pane.poll();
        pane.run(UpdatesEffect::CheckForUpdates, None);
        // The check is held until released below: were `sync` to wait for
        // it, this call would only return once the gate timed out.
        pane.poll();
        assert!(pane.check_pending, "the check runs off the UI thread");
        assert_eq!(pane.prefs().last_checked, "Never");
        release.send(()).unwrap();
        settle(&mut pane, |pane| pane.check_pending);
        assert!(
            !control.gate_timed_out(),
            "sync blocked on the update check"
        );
        assert_eq!(pane.prefs().last_checked, "CHECKED-NOW");
        assert!(pane.notice().is_none());
    }

    // Case `failed_async_update_check_does_not_stamp_last_checked`.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(None, None).failing_check());
        let mut pane = pane_with_control(control);
        pane.poll();
        pane.run(UpdatesEffect::CheckForUpdates, None);
        settle(&mut pane, |pane| pane.check_pending);
        assert_eq!(pane.prefs().last_checked, "Never");
        assert!(
            pane.notice()
                .is_some_and(|message| message.contains("could not be checked"))
        );
    }

    // Case `blocking_firmware_refresh_returns_promptly_and_reports_success_or_failure`.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(None, None));
        let release = control.gate_refreshes();
        let mut pane = pane_with_control(Arc::clone(&control));
        pane.poll();
        pane.run(UpdatesEffect::RefreshFirmware, None);
        pane.poll();
        assert!(pane.firmware_refresh_pending);
        release.send(()).unwrap();
        settle(&mut pane, |pane| pane.firmware_refresh_pending);
        assert!(
            !control.gate_timed_out(),
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
        assert!(pane.notice().is_some_and(|message| {
            message.contains("Firmware metadata could not be refreshed")
        }));
    }

    // Case `duplicate_update_checks_are_cleared_while_one_is_pending`.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(None, None));
        let release = control.gate_checks();
        let mut pane = pane_with_control(Arc::clone(&control));
        pane.poll();
        pane.run(UpdatesEffect::CheckForUpdates, None);
        pane.poll();
        assert!(pane.check_pending);
        // A second press while the first check is provably still running.
        pane.run(UpdatesEffect::CheckForUpdates, None);
        pane.poll();
        release.send(()).unwrap();
        settle(&mut pane, |pane| pane.check_pending);
        assert_eq!(*control.check_calls.lock().unwrap(), 1);
    }
}

/// Drive the host's polling until `pending` clears. The bound is a hang
/// guard for a worker that never reports, not a performance budget.
fn settle(pane: &mut UpdatesModel, pending: impl Fn(&UpdatesModel) -> bool) {
    let deadline = ui::time::now() + GATE_TIMEOUT;
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

/// Restart reboots once; rollback is confirmed, reported, honestly described and absent without an image.
#[test]
fn restart_and_rollback_are_confirmed_and_reported() {
    // Case `restart_reboots_once`.
    // Restart & update reboots exactly once via the injected rebooter.
    {
        let reb = Arc::new(RecordingRebooter::default());
        let mut pane = pane_with(Some("Tilo 2026.07"), reb.clone());
        pane.poll();
        pane.run(UpdatesEffect::RestartToUpdate, None);
        pane.poll();
        // A second stray press must not reboot again.
        pane.run(UpdatesEffect::RestartToUpdate, None);
        pane.poll();
        settle(&mut pane, UpdatesModel::working);
        assert_eq!(*reb.calls.lock().unwrap(), 1);
    }

    // Case `rolling_back_needs_a_confirmation_and_then_acts`.
    // Rolling back takes two clicks, and the first one only shows what a
    // rollback does not undo.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(Some(true), Some("tilo:gen41")));
        let mut pane = pane_with_control(control.clone());
        pane.poll();
        assert_eq!(pane.rollback_target(), Some("tilo:gen41"));

        // First click: nothing happens to the machine.
        pane.run(UpdatesEffect::ReviewRollback, None);
        pane.poll();
        assert_eq!(
            *control.rollbacks.lock().unwrap(),
            0,
            "the first click must only ask; a one-click rollback is a rollback \
             by accident"
        );

        // The confirmation view has to actually carry the caveats, or the
        // second click is uninformed.
        let shown = pane.rollback_card_lines().join("\n");
        for phrase in ["do NOT revert", "next app update"] {
            assert!(
                shown.contains(phrase),
                "the confirmation must say what a rollback does not undo \
                 ({phrase} missing) — shown was:\n{shown}"
            );
        }

        // Second click: it acts.
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

    // Case `a_failed_rollback_is_reported_in_the_pane`.
    // A failed rollback says so where the person is looking.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(Some(true), Some("tilo:gen41")).failing_rollback());
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

    // Case `the_help_text_does_not_promise_what_is_not_offered`.
    // The help text must never promise a capability the pane does not offer
    // — assert on what the pane renders, not on a comment.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_with_control(Arc::new(FakeControl::new(Some(true), Some("tilo:gen41"))));
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

    // Case `no_previous_image_is_stated_rather_than_offered`.
    // A machine with nothing to go back to says so, rather than offering a
    // button that would fail.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_with_control(Arc::new(FakeControl::new(Some(true), None)));
        let shown = pane.rollback_card_lines().join("\n");
        assert!(
            shown.contains("no previous image on this machine yet"),
            "the absence has to be stated; an absent card reads as an absent \
             feature. Shown was:\n{shown}"
        );
    }
}

// ---- timer + rollback control -------------------------------------------

/// A machine, recorded. `timer` is what `systemctl is-enabled` would say.
struct FakeControl {
    timer: Mutex<Option<bool>>,
    set_calls: Mutex<Vec<bool>>,
    rollbacks: Mutex<u32>,
    target: Mutex<Option<String>>,
    set_fails: bool,
    rollback_fails: bool,
    check_gate: Mutex<Option<channel::Receiver<()>>>,
    check_fails: bool,
    refresh_gate: Mutex<Option<channel::Receiver<()>>>,
    refresh_fails: Mutex<bool>,
    check_calls: Mutex<u32>,
    gate_timed_out: std::sync::atomic::AtomicBool,
}

/// How long a gated fake waits for its release before giving up and
/// recording that the caller held the UI thread.
const GATE_TIMEOUT: Duration = Duration::from_secs(30);

impl FakeControl {
    fn new(timer: Option<bool>, target: Option<&str>) -> Self {
        Self {
            timer: Mutex::new(timer),
            set_calls: Mutex::new(Vec::new()),
            rollbacks: Mutex::new(0),
            target: Mutex::new(target.map(str::to_string)),
            set_fails: false,
            rollback_fails: false,
            check_gate: Mutex::new(None),
            check_fails: false,
            refresh_gate: Mutex::new(None),
            refresh_fails: Mutex::new(false),
            check_calls: Mutex::new(0),
            gate_timed_out: std::sync::atomic::AtomicBool::new(false),
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
    fn failing_check(mut self) -> Self {
        self.check_fails = true;
        self
    }
    /// Hold every update check until the returned sender releases it.
    fn gate_checks(&self) -> channel::Sender<()> {
        let (release, gate) = channel::channel();
        *self.check_gate.lock().unwrap() = Some(gate);
        release
    }
    /// Hold every firmware refresh until the returned sender releases it.
    fn gate_refreshes(&self) -> channel::Sender<()> {
        let (release, gate) = channel::channel();
        *self.refresh_gate.lock().unwrap() = Some(gate);
        release
    }
    fn wait_at(&self, gate: &Mutex<Option<channel::Receiver<()>>>) {
        if let Some(gate) = gate.lock().unwrap().as_ref()
            && gate.recv_timeout(GATE_TIMEOUT).is_err()
        {
            self.gate_timed_out
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    fn gate_timed_out(&self) -> bool {
        self.gate_timed_out
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
    fn rollback_target(&self) -> Result<Option<String>, String> {
        crate::device_read::off_ui_thread();
        Ok(self.target.lock().unwrap().clone())
    }
    fn roll_back(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        *self.rollbacks.lock().unwrap() += 1;
        if self.rollback_fails {
            return Err("the boot loader would not take it".into());
        }
        *self.target.lock().unwrap() = None;
        Ok(())
    }

    fn check_now(&self) -> Result<(), String> {
        crate::device_read::off_ui_thread();
        *self.check_calls.lock().unwrap() += 1;
        self.wait_at(&self.check_gate);
        if self.check_fails {
            Err("update check failed".into())
        } else {
            Ok(())
        }
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
        self.wait_at(&self.refresh_gate);
        if *self.refresh_fails.lock().unwrap() {
            Err("firmware refresh failed".into())
        } else {
            Ok((Vec::new(), Vec::new()))
        }
    }
}

/// `bootc` that cannot be asked (not root, absent, refused) is reported
/// as that failure. "Your system is up to date" is a claim about the
/// machine and is never made from a failed read.
#[test]
fn an_unreadable_bootc_status_is_not_reported_as_up_to_date() {
    let mut app = ui::prelude::TestApp::<crate::resource::SettingsShell>::new().unwrap();
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: None,
            staged_error: Some("error: This command requires root privileges".into()),
        };
        let requeried = status.clone();
        *crate::panes::updates::area(settings)
            .unwrap()
            .model()
            .borrow_mut() = UpdatesModel::with_parts_and_control(
            settings.sections().clone(),
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            Arc::new(RecordingRebooter::default()),
            Arc::new(FakeControl::new(Some(true), None)),
        );
    })
    .expect("the Settings window is mounted");
    crate::effects::show_pane(&mut app, crate::categories::Category::Updates);
    app.settle();
    let frame = app.harness().inspection_json().unwrap();
    assert!(
        frame.contains("Can't check for updates: error: This command requires root privileges"),
        "{frame}"
    );
    for claim in [t("upd-up-to-date"), t("upd-os-current")] {
        assert!(!frame.contains(&claim), "claims {claim:?}: {frame}");
    }
}

fn pane_with_control(control: Arc<FakeControl>) -> UpdatesModel {
    let status = UpdateStatus {
        current_version: "Tilo 2026.06".into(),
        staged_version: None,
        staged_error: None,
    };
    let requeried = status.clone();
    UpdatesModel::with_parts_and_control(
        test_cx(),
        status,
        UpdatePrefs::default(),
        Arc::new(move || requeried.clone()),
        Arc::new(|| "CHECKED-NOW".to_string()),
        Arc::new(RecordingRebooter::default()),
        control,
    )
}

/// The switch mirrors the timer, disables it, snaps back on refusal and is absent when unreadable.
#[test]
fn automatic_update_switch_follows_the_timer() {
    // Case `the_switch_opens_showing_what_the_timer_actually_is`.
    // The switch shows the timer, not a stored preference, even when the two
    // disagree.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        // A preferences file that says "off" and a timer that is on.
        let control = Arc::new(FakeControl::new(Some(true), None));
        let pane = UpdatesModel::with_parts_and_control(
            test_cx(),
            UpdateStatus {
                current_version: "Tilo".into(),
                staged_version: None,
                staged_error: None,
            },
            UpdatePrefs {
                auto: false,
                ..UpdatePrefs::default()
            },
            Arc::new(|| UpdateStatus {
                current_version: "Tilo".into(),
                staged_version: None,
                staged_error: None,
            }),
            Arc::new(|| "now".to_string()),
            Arc::new(RecordingRebooter::default()),
            control,
        );
        assert!(
            pane.prefs().auto,
            "the machine's timer wins over a stale preference; showing the \
             preference is how a switch comes to lie about the system"
        );
        assert_eq!(pane.timer_enabled(), Some(true));
    }

    // Case `turning_automatic_updates_off_disables_the_timer`.
    // Turning the switch off turns the timer off.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(Some(true), None));
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

    // Case `a_refused_switch_snaps_back_and_is_reported_in_the_pane`.
    // When the system refuses, the switch goes back and the pane says so on
    // screen. A warning in the journal is, for a settings switch, the same as
    // failing silently.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let control = Arc::new(FakeControl::new(Some(true), None).failing_set());
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

    // Case `an_unreadable_timer_is_not_rendered_as_off`.
    // A machine that cannot be asked gets no switch at all. Rendering "off"
    // for a timer that could not be read is the same class of lie.
    {
        let _cfg = crate::prefs::test_env::ConfigGuard::new();
        let pane = pane_with_control(Arc::new(FakeControl::new(None, None)));
        assert_eq!(pane.timer_enabled(), None);
        // And the stub used by the older constructor answers the same way, so
        // a test written before this existed cannot get a confident `false`.
        assert_eq!(UnavailableControl.timer_enabled(), None);
        assert!(UnavailableControl.set_timer(true).is_err());
        assert!(UnavailableControl.roll_back().is_err());
    }
}

/// What the machine reported after a check or a change of the timer is
/// kept in the update preferences, through the section's verbs.
#[test]
fn a_check_and_the_timer_are_kept_in_the_update_preferences() {
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    let mut app = TestApp::<SettingsShell>::new().unwrap();
    app.with_widget::<crate::window::Inspected, _>(|settings| {
        let status = UpdateStatus {
            current_version: "Tilo 2026.06".into(),
            staged_version: None,
            staged_error: None,
        };
        let requeried = status.clone();
        *crate::panes::updates::area(settings)
            .unwrap()
            .model()
            .borrow_mut() = UpdatesModel::with_parts_and_control(
            settings.sections().clone(),
            status,
            UpdatePrefs::default(),
            Arc::new(move || requeried.clone()),
            Arc::new(|| "CHECKED-NOW".to_string()),
            Arc::new(RecordingRebooter::default()),
            Arc::new(FakeControl::new(Some(true), None)),
        );
    })
    .expect("the Settings window is mounted");
    app.effect(&SettingsEffect::from(UpdatesEffect::CheckForUpdates))
        .expect("the check finishes");
    app.effect(&SettingsEffect::from(UpdatesEffect::SetAutomaticUpdates {
        on: false,
    }))
    .expect("the timer turns off");
    app.settle();
    let kept = app.settings::<UpdatePrefs>().get();
    assert_eq!(kept.last_checked, "CHECKED-NOW");
    assert!(!kept.auto, "the timer the machine reports is kept");
}

/// The effects reach the model the app keeps; a capture's reaches no
/// system tool and says why it did nothing.
#[test]
fn effects_reach_the_model_and_report_how_they_end() {
    use crate::effects::SettingsEffect;
    use crate::resource::SettingsShell;
    use ui::prelude::TestApp;

    let mut app = TestApp::<SettingsShell>::new().unwrap();
    app.effect(&SettingsEffect::from(UpdatesEffect::CheckForUpdates))
        .expect("the check reads the sample image");
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

/// A control whose every call waits for its gate to open: a device that
/// takes as long as it likes. The gate gives up after ten seconds so a
/// broken test ends.
struct GatedControl {
    gate: Arc<crate::stall::Stall>,
    timer: Mutex<Option<bool>>,
    target: Mutex<Option<String>>,
    set_calls: Mutex<Vec<bool>>,
    rollbacks: Mutex<u32>,
}

impl GatedControl {
    fn pass(&self) {
        crate::device_read::off_ui_thread();
        self.gate.wait();
    }
}

impl UpdateControl for GatedControl {
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
    fn rollback_target(&self) -> Result<Option<String>, String> {
        self.pass();
        Ok(self.target.lock().unwrap().clone())
    }
    fn roll_back(&self) -> Result<(), String> {
        self.pass();
        *self.rollbacks.lock().unwrap() += 1;
        *self.target.lock().unwrap() = None;
        Ok(())
    }
}

/// The pane opens at once over a device that has not answered, says it
/// is reading, and a request made meanwhile runs once the read lands. A
/// change the device takes long to make never holds the window either.
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
    let gate = crate::stall::Stall::new();
    let control = Arc::new(GatedControl {
        gate: Arc::clone(&gate),
        timer: Mutex::new(Some(true)),
        target: Mutex::new(Some("tilo:gen41".into())),
        set_calls: Mutex::new(Vec::new()),
        rollbacks: Mutex::new(0),
    });
    let readers = DeviceReaders {
        status: Arc::new(|| {
            crate::device_read::off_ui_thread();
            UpdateStatus {
                current_version: "Tilo 2026.06".into(),
                staged_version: None,
                staged_error: None,
            }
        }),
        details: Arc::new(|_| UpdateDetails::default()),
        control: Arc::clone(&control) as Arc<dyn UpdateControl>,
        rebooter: Arc::new(RecordingRebooter::default()),
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
            *shared.borrow_mut() =
                UpdatesModel::reading(settings.storage(), settings.sections().clone(), readers);
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

    gate.release();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(30), |harness| {
                model.borrow().timer_enabled() == Some(false)
                    && harness
                        .inspection_json()
                        .is_ok_and(|frame| frame.contains(&t("upd-up-to-date")))
            }),
        "the read lands, then the waiting request turns the timer off"
    );
    assert_eq!(*control.set_calls.lock().unwrap(), vec![false]);

    // A rollback the device takes long to make.
    gate.hold();
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
    assert_eq!(model.borrow().rollback_target(), Some("tilo:gen41"));

    gate.release();
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(30), |_| model
                .borrow()
                .notice()
                .is_some_and(|notice| notice.contains("Restart"))),
        "the rollback's outcome is shown"
    );
    assert_eq!(*control.rollbacks.lock().unwrap(), 1);
    assert_eq!(model.borrow().rollback_target(), None);
}
