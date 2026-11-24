use super::*;
use crate::hosting::testing::TestHost;
use automerge::AutoCommit;
use tilo_resource::Fields as _;

fn harness(app: ImagerApp, doc: AutoCommit, viewport: (u32, u32)) -> ui::headless::FrameHarness {
    let mut state = ui::State::new();
    assert!(state.adopt_doc(doc));
    ui::headless::FrameHarness::for_root(state, app, viewport, Theme::dark())
}
fn ids(app: &ImagerApp) -> Vec<String> {
    fn walk(w: &dyn Widget, out: &mut Vec<String>) {
        if let Some(id) = w.test_id() {
            out.push(id.into());
        }
        for c in w.children() {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    walk(app.build_composition().as_ref(), &mut out);
    out
}

fn present(app: &mut ImagerApp, doc: &AutoCommit) {
    use tilo_resource::Resource as _;
    app.present_state(&Imager::read(doc).expect("imager state"));
    let mut model = doc.clone();
    app.sync(&mut model, &automerge::ROOT);
}

fn document(state: &Imager) -> AutoCommit {
    let mut doc = AutoCommit::new();
    state.write_fields(&mut doc, &automerge::ROOT).unwrap();
    doc
}

fn demo_state() -> Imager {
    Imager {
        source: Some(crate::model::sample_source()),
        recent: crate::model::sample_recent_sources()
            .into_iter()
            .enumerate()
            .map(|(rank, source)| crate::model::RecentSource {
                id: tilo_resource::Id::from_raw(source.id()),
                rank: rank as i64,
                source,
            })
            .collect(),
        profile: InstallProfile::default(),
    }
}

fn seeded_app() -> (ImagerApp, AutoCommit) {
    let mut app = ImagerApp::new();
    app.targets = crate::model::sample_targets();
    app.releases = crate::model::sample_releases();
    let doc = document(&demo_state());
    present(&mut app, &doc);
    (app, doc)
}

#[test]
fn unchanged_disk_review_discloses_the_authored_hostname() {
    use tilo_resource::Resource as _;

    let (mut app, mut doc) = seeded_app();
    let mut source = app
        .recent
        .iter()
        .find(|source| source.kind == SourceKind::IsoFile)
        .unwrap()
        .clone();
    source.installable = false;
    let mut state = Imager::read(&doc).unwrap();
    state.select_source(source);
    state.write_fields(&mut doc, &automerge::ROOT).unwrap();
    present(&mut app, &doc);
    app.goto(Step::Confirm);
    assert!(!app.customizable());
    assert_eq!(
        app.review_setup(),
        crate::setup::strings::t("imager-review-unchanged")
    );

    Imager::paths()
        .profile
        .hostname
        .replace("reviewed-host")
        .apply_local(&mut doc)
        .unwrap();
    present(&mut app, &doc);
    assert_eq!(
        crate::handoff::chosen_hostname(&app.profile),
        Some("reviewed-host")
    );
    let setup = app
        .review_rows()
        .into_iter()
        .find(|(label, _)| *label == crate::setup::strings::t("imager-review-setup"))
        .unwrap()
        .1;
    assert!(setup.contains("reviewed-host"), "{setup}");
    assert!(setup.contains("then verified"), "{setup}");
    assert!(!setup.contains("Pre-configured"), "{setup}");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn startup_state_is_blank_local_and_reconciled_before_writes() {
    {
        let (mut app, mut doc) = seeded_app();
        let before = doc.save();
        app.selected = Some(0);
        app.goto(Step::Confirm);
        app.progress = Progress {
            phase: JobPhase::Writing,
            write_frac: 0.5,
            verify_frac: 0.0,
            rate_bps: 1,
            error: None,
        };
        app.set_status("local writer status");
        assert_eq!(
            doc.save(),
            before,
            "selection, station, status, and job progress are local"
        );
        let peer = AutoCommit::load(&before).unwrap();
        let mut peer_app = ImagerApp::new();
        present(&mut peer_app, &peer);
        assert_eq!(peer_app.step, Step::Source);
        assert_eq!(peer_app.selected, None);
        assert_eq!(peer_app.progress.phase, JobPhase::Idle);
        assert!(peer_app.status.is_empty());
    }

    {
        let (app, _doc) = seeded_app();
        assert_eq!(app.targets.len(), 2);
        assert!(app.source.is_some());
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.recent.len(), 3);
    }

    {
        let mut app = ImagerApp::new();
        let doc = AutoCommit::new();
        present(&mut app, &doc);
        assert!(app.targets.is_empty(), "no fabricated drives");
        assert!(app.source.is_none(), "no fabricated image");
        assert!(app.recent.is_empty(), "no fabricated recent history");
        assert_eq!(app.step, Step::Source);
    }

    {
        let (mut app, mut doc) = seeded_app();
        app.select_target(0);
        app.step = Step::Write;
        app.progress = Progress {
            phase: JobPhase::Writing,
            write_frac: 0.8,
            verify_frac: 0.0,
            rate_bps: 64_000_000,
            error: None,
        };
        app.status = "Writing locally".into();
        let saved = doc.save();
        let mut restarted = ImagerApp::new();
        let restored_doc = AutoCommit::load(&saved).unwrap();
        present(&mut restarted, &restored_doc);
        assert_eq!(restarted.step, Step::Source);
        assert_eq!(restarted.progress.phase, JobPhase::Idle);
        assert!(restarted.selected.is_none());
        assert!(restarted.status.is_empty());
        assert!(!restarted.can_advance_with_writer(Step::Drive, true));
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        app.startup_write_state_reconciled = false;

        assert!(!app.can_advance_with_writer(Step::Confirm, true));
        let error = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("unreconciled startup must not expose a write");
        assert!(error.contains("Restoring the previous image-write state"));
        let mut rows = vec![app.target_row(app.target().unwrap()).into_resource()];
        let statuses = crate::hosting::Window::row_statuses(&app);
        let said = |id: &str| {
            statuses
                .iter()
                .find(|(row, _)| row == id)
                .map(|(_, statuses)| statuses.clone())
                .unwrap_or_default()
        };
        tilo_resource::status::apply_row_statuses(&mut rows, &said);
        let write = rows[0]
            .actions
            .iter()
            .find(|spec| spec.id == "write")
            .unwrap();
        let tilo_actions::Availability::Unavailable { code, .. } = &write.contract.availability
        else {
            panic!("the drive's write waits for the startup reconcile");
        };
        assert_eq!(code, "restoring_write_state");
    }

    {
        let mut app = ImagerApp::new();
        let doc = AutoCommit::new();
        present(&mut app, &doc);
        assert!(app.releases().is_empty(), "no fabricated Tilo releases");

        let offered = ids(&app).iter().any(|id| id.starts_with("imager.release."));
        assert!(
            !offered,
            "no release row may be hit-testable on the live path"
        );
        assert!(
            app.source.is_none(),
            "and nothing is pre-selected as the source"
        );
    }

    {
        let (app, _doc) = seeded_app();
        assert_eq!(app.releases().len(), 2);
        let offered = ids(&app)
            .iter()
            .filter(|id| id.starts_with("imager.release."))
            .count();
        assert_eq!(offered, 2, "each seeded release has a row to click");
    }
}

#[test]
fn the_wizards_buttons_read_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    catalog.set_locale("de-DE");
    let mut labels = Vec::new();
    for step in [Step::Source, Step::Customize] {
        let (mut app, doc) = seeded_app();
        app.step = step;
        let host = harness(app, doc, (960, 720));
        labels.extend(
            host.a11y_nodes()
                .into_iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned)),
        );
    }
    catalog.set_locale("en-US");
    for german in ["Zurück", "Weiter", "Durchsuchen…"] {
        assert!(
            labels.iter().any(|label| label == german),
            "{german} is missing from {labels:?}"
        );
    }
    assert!(
        labels
            .iter()
            .all(|label| !matches!(label.as_str(), "Back" | "Continue" | "Browse…")),
        "the source language is left in the German window: {labels:?}"
    );
}

#[test]
fn the_customization_form_reads_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let mut host = TestHost::capture(Some("customize"));
    let labels: Vec<String> = host
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect();
    catalog.set_locale(&previous);
    for german in [
        "Identität",
        "Region",
        "Speicher",
        "Hostname",
        "Vollständiger Name",
        "Benutzername",
        "Sprache und Region",
        "Zeitzone",
        "Tastaturbelegung",
        "Dateisystem",
    ] {
        assert!(
            labels.iter().any(|label| label == german),
            "{german} is missing from {labels:?}"
        );
    }
    assert!(
        labels.iter().all(|label| !matches!(
            label.as_str(),
            "Identity"
                | "Storage"
                | "Full name"
                | "Username"
                | "Language and locale"
                | "Time zone"
                | "Keyboard layout"
                | "Filesystem"
        )),
        "the source language is left in the German window: {labels:?}"
    );
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn wizard_semantics_labels_live_regions_and_keyboard_order() {
    use ui::a11y::Action as AccessAction;
    let (mut app, _) = seeded_app();
    let mut generic = crate::model::sample_releases().remove(0);
    generic.architecture = "aarch64".into();
    let mut board = generic.clone();
    board.hardware_profile = "lenovo-83ed".into();
    assert_ne!(generic.selection_id(), board.selection_id());
    app.releases = vec![generic, board.clone()];
    assert!(ids(&app).contains(&format!("imager.release.{}", board.selection_id())));
    app.source = Some(board.to_source());
    assert!(board.matches_source(app.source.as_ref().unwrap()));
    assert!(!app.releases[0].matches_source(app.source.as_ref().unwrap()));
    assert!(
        app.review_rows()
            .into_iter()
            .find(|(label, _)| *label == crate::setup::strings::t("imager-review-image"))
            .unwrap()
            .1
            .contains("lenovo-83ed only")
    );
    let mut host = TestHost::capture(Some("source"));
    let tree = host.harness().a11y_nodes();
    assert!(
        tree.iter()
            .any(|(_, n)| n.role() == ui::accesskit::Role::Heading
                && n.label() == Some("Choose an image"))
    );
    let stepper = tree
        .iter()
        .find(|(_, n)| {
            n.role() == ui::accesskit::Role::ProgressIndicator && n.label() == Some("Source")
        })
        .unwrap();
    assert_eq!(stepper.1.value(), Some("Step 1 of 6"));
    let mut order = Vec::new();
    for _ in 0..16 {
        host.harness().tab(false);
        if let Some(n) = host.harness().focused_access_node() {
            order.push(n.name.unwrap_or_default());
        }
    }
    let browse = order
        .iter()
        .position(|n| n == "Browse…")
        .expect("browse is keyboard reachable");
    let next = order
        .iter()
        .position(|n| n == "Continue")
        .expect("continue is keyboard reachable");
    assert!(browse < next);
    let mut host = TestHost::capture(Some("customize"));
    for (id, role) in [
        ("imager.profile.hostname", ui::a11y::Role::TextInput),
        ("imager.profile.locale", ui::a11y::Role::ComboBox),
        ("imager.profile.timezone", ui::a11y::Role::ComboBox),
        ("imager.profile.keyboard", ui::a11y::Role::ComboBox),
        ("imager.profile.filesystem", ui::a11y::Role::ComboBox),
    ] {
        host.harness()
            .expect(&ui::headless::SemanticExpectation::named(id).role(role))
            .unwrap();
    }
    host.harness()
        .perform_named(
            "imager.profile.hostname",
            &AccessAction::SetValue("accessible-host".into()),
        )
        .unwrap();
    assert_eq!(host.state().profile.hostname, "accessible-host");
    let mut order = Vec::new();
    for _ in 0..24 {
        host.harness().tab(false);
        if let Some(n) = host.harness().focused_access_node() {
            order.push(n.name.unwrap_or_default());
        }
    }
    for name in [
        "Hostname",
        "Full name",
        "Username",
        "Time zone",
        "Language and locale",
        "Keyboard layout",
        "Filesystem",
        "Back",
        "Continue",
    ] {
        assert!(order.iter().any(|n| n == name), "{name}: {order:?}");
    }
    let mut host = TestHost::capture(Some("confirm"));
    assert!(!host.harness().targets().contains_key("imager.confirm"));
    assert!(host.harness().targets().contains_key("imager.write"));
    let mut host = TestHost::capture(Some("progress"));
    assert!(host.harness().a11y_nodes().iter().any(|(_, n)| n.role()
        == ui::accesskit::Role::ProgressIndicator
        && n.value().is_some()));
}

#[test]
fn the_writers_progress_and_stations_read_in_the_windows_language() {
    let catalog = &crate::resource::CATALOG;
    let previous = catalog.current_locale();
    catalog.set_locale("de-DE");
    let mut app = ImagerApp::new();
    app.action = Action::Install;
    let (_, caption) = app.phase();
    app.action = Action::Flash;
    let (_, writing) = app.phase();
    let stations: Vec<String> = Step::all()
        .iter()
        .map(|step| crate::setup::strings::t(step_message(*step)))
        .collect();
    let eta = crate::setup::strings::t_args(
        "imager-writing-eta",
        &[("time", ui::i18n::ArgValue::Text("3 Minuten".into()))],
    );
    catalog.set_locale(&previous);
    assert_eq!(caption, "Wird installiert");
    assert_eq!(writing, "Wird geschrieben");
    assert_eq!(
        stations,
        [
            "Quelle",
            "Laufwerk",
            "Anpassen",
            "Prüfen",
            "Schreiben",
            "Fertig"
        ]
    );
    assert_eq!(eta, "noch etwa 3 Minuten");
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn step_rail_keeps_six_stations_inside_the_viewport() {
    assert_eq!(ImagerApp::new().visible_steps(), Step::all());
    assert_eq!(stepper_labels().len(), 6);
    assert_eq!(stepper_labels().first().map(String::as_str), Some("Source"));
    assert_eq!(stepper_labels().last().map(String::as_str), Some("Done"));
    for viewport in [(1280, 800), (320, 640)] {
        let (app, doc) = seeded_app();
        let h = harness(app, doc, viewport);
        let stepper = h
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| {
                n.role() == ui::accesskit::Role::ProgressIndicator && n.label() == Some("Source")
            })
            .unwrap()
            .1;
        assert_eq!(stepper.value(), Some("Step 1 of 6"));
        let bounds = stepper.bounds().unwrap();
        assert!(bounds.x0 >= 0.0 && bounds.x1 <= f64::from(viewport.0));
        assert!(
            bounds.height()
                >= f64::from(Theme::dark().text(ui::TextRole::Caption).line_height_px()),
            "the passive progress label fits its measured line"
        );
        assert!(
            h.targets()["imager.next"].height() >= 40.0,
            "wizard navigation retains its accessible target floor"
        );
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn step_body_and_footer_reflow_without_overlap() {
    let mut heading_heights = Vec::new();
    for scale in [1.0, 2.0] {
        let (app, doc) = seeded_app();
        let mut h = harness(app, doc, (1200, 800));
        h.relayout_themed((1200, 800), Theme::dark().with_text_scale(scale))
            .unwrap();
        let heading = h
            .a11y_nodes()
            .into_iter()
            .find(|(_, n)| {
                n.role() == ui::accesskit::Role::Heading && n.label() == Some("Choose an image")
            })
            .unwrap()
            .1
            .bounds()
            .unwrap();
        heading_heights.push(heading.height());
        let browse = h.targets()["imager.browse"];
        assert!(
            browse.y0 >= heading.y1,
            "content clears the measured heading at {scale}x"
        );
    }
    assert!(
        heading_heights[1] > heading_heights[0] * 1.5,
        "the heading's measured height responds to larger type"
    );

    for viewport in [(1280, 800), (800, 600), (320, 640)] {
        for scenario in [
            "source",
            "drive",
            "drive-empty",
            "customize",
            "confirm",
            "progress",
            "done",
            "done-failed",
        ] {
            let mut host = TestHost::capture(Some(scenario));
            host.harness()
                .resize(viewport)
                .unwrap_or_else(|error| panic!("{scenario} at {viewport:?}: {error:?}"));
            let mut back = None;
            let mut primary = None;
            let mut stepper = None;
            for (_, n) in host.harness().a11y_nodes() {
                if n.role() == ui::accesskit::Role::Window {
                    continue;
                }
                let Some(r) = n.bounds() else {
                    continue;
                };
                assert!(
                    r.x0 >= -0.5 && r.x1 <= f64::from(viewport.0) + 0.5,
                    "{scenario} {viewport:?} {:?}: {r:?}",
                    n.label()
                );
                if n.label() == Some("Back") {
                    back = Some(r);
                }
                if matches!(
                    n.label(),
                    Some("Continue" | "Write image" | "Write another")
                ) {
                    primary = Some(r);
                }
                if n.role() == ui::accesskit::Role::ProgressIndicator
                    && matches!(
                        n.label(),
                        Some("Source" | "Drive" | "Customize" | "Review" | "Write" | "Done")
                    )
                {
                    stepper = Some(r);
                }
            }
            let stepper = stepper.expect("each step retains its position indicator");
            assert!(stepper.y0 >= -0.5 && stepper.y1 <= f64::from(viewport.1) + 0.5);
            if let (Some(back), Some(primary)) = (back, primary) {
                assert!(back.x1 <= primary.x0 || primary.x1 <= back.x0);
            }
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn wizard_advance_and_confirm_require_valid_choices_and_capability() {
    {
        let (mut app, _doc) = seeded_app();
        app.advance();
        assert_eq!(app.step, Step::Drive);
        app.advance();
        assert_eq!(app.step, Step::Drive);
        app.select_target(0);
        app.advance();
        assert_eq!(app.step, Step::Customize);
        app.advance();
        assert_eq!(app.step, Step::Confirm);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        app.goto(Step::Confirm);

        assert!(crate::write::WriteBackend::PrivilegedHelper.is_available());
        assert!(app.can_advance_with_writer(
            Step::Confirm,
            crate::write::WriteBackend::PrivilegedHelper.is_available()
        ));
        assert!(!app.can_advance_with_writer(
            Step::Confirm,
            crate::write::WriteBackend::Disabled.is_available()
        ));
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        app.profile.hostname = "bad host".into();
        app.goto(Step::Customize);

        app.advance();
        assert_eq!(app.step, Step::Customize);
        assert!(app.status.contains("Hostname"));

        app.goto(Step::Confirm);
        let error = app
            .write_request(
                app.source.clone().unwrap(),
                app.target().unwrap().clone(),
                None,
            )
            .expect_err("an invalid profile refuses the write request");
        assert_eq!(app.step, Step::Confirm);
        assert!(app.job_control.is_none());
        assert!(error.contains("Hostname"));
    }

    {
        let (mut app, _doc) = seeded_app();
        let iso = app
            .recent
            .iter()
            .find(|s| s.kind == SourceKind::IsoFile)
            .cloned()
            .unwrap();
        app.select_source(&iso);
        app.advance(); // → Drive
        app.select_target(0);
        app.advance(); // installable == false → straight to Confirm
        assert_eq!(app.step, Step::Confirm);
        assert!(!app.customizable());
    }

    {
        let (mut app, _doc) = seeded_app();
        let official = crate::model::sample_source();
        let mut local = official.clone();
        local.kind = SourceKind::IsoFile;
        local.path = "/tmp/tilo-official-looking.iso".into();
        local.identity = None;
        app.select_source(&local);
        assert!(!app.customizable());
        assert_eq!(app.action, Action::Flash);

        app.source = Some(official);
        assert!(app.customizable());
    }

    {
        let (mut app, _doc) = seeded_app();
        assert!(app.customizable(), "seeded source is official");
        assert!(
            app.offers_write_setup(),
            "official Confirm must collect Wi-Fi and enrollment"
        );
        app.select_target(0);
        app.goto(Step::Confirm);
        app.profile.wifi_ssid = "studio".into();
        app.wifi_psk = "correct-horse".into();
        let err = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("privileged writer is off in unit tests");
        assert!(
            !err.to_lowercase().contains("installer stick"),
            "official installable write must not refuse handoff: {err}"
        );
        assert!(
            err.to_lowercase().contains("disabled") || err.to_lowercase().contains("backend"),
            "expected write-disabled after staging handoff, got: {err}"
        );
        assert!(app.job_control.is_none());
        assert_eq!(app.step, Step::Confirm);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0); // sdb
        app.goto(Step::Confirm);
        let request = app
            .write_request(
                app.source.clone().unwrap(),
                app.target().unwrap().clone(),
                None,
            )
            .unwrap();
        let (start, receiver, control) =
            crate::write::spawn_job(request, "wrong", crate::write::WriteBackend::Disabled);
        assert_eq!(start, crate::write::JobStart::ConfirmationMismatch);
        assert!(receiver.is_none());
        assert!(control.is_none());
        assert_eq!(app.progress.phase, JobPhase::Idle, "no job started");
        assert!(app.job_control.is_none());
        assert_eq!(app.step, Step::Confirm, "stays on the Confirm station");
    }
}

#[test]
fn write_setup_wifi_and_owner_key_stage_without_identity_enrollment() {
    let (mut app, mut doc) = seeded_app();
    app.select_target(0);
    app.goto(Step::Confirm);
    Imager::paths()
        .profile
        .wifi_ssid
        .replace("studio")
        .apply_local(&mut doc)
        .unwrap();
    Imager::paths()
        .profile
        .enroll_local_identity
        .set(false)
        .apply_local(&mut doc)
        .unwrap();
    present(&mut app, &doc);
    app.wifi_psk = "correct-horse".into();
    let key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA owner@example";
    app.ssh_public_key.set(key.into());
    assert!(!app.profile.enroll_local_identity);
    let error = app
        .begin_write(&app.target().unwrap().dev_path(), "sdb")
        .expect_err("access staging reaches the disabled writer without enrollment");
    assert!(
        error.to_lowercase().contains("disabled") || error.to_lowercase().contains("backend"),
        "expected the disabled write backend after staging access: {error}"
    );
    assert!(app.job_control.is_none());
    assert_eq!(app.step, Step::Confirm);
    let view = harness(app, doc, (1180, 820));
    assert!(
        view.a11y_nodes()
            .iter()
            .any(|(_, node)| { node.value() == Some(key) }),
        "the owner SSH key remains visible in the production Confirm form"
    );
}

#[test]
fn write_setup_missing_wifi_password_is_reported_before_identity_lookup() {
    let (mut app, mut doc) = seeded_app();
    app.select_target(0);
    app.goto(Step::Confirm);
    Imager::paths()
        .profile
        .wifi_ssid
        .replace("studio")
        .apply_local(&mut doc)
        .unwrap();
    Imager::paths()
        .profile
        .enroll_local_identity
        .set(true)
        .apply_local(&mut doc)
        .unwrap();
    present(&mut app, &doc);
    let error = app
        .begin_write(&app.target().unwrap().dev_path(), "sdb")
        .expect_err("Wi-Fi without a password must be refused before enrollment");
    assert!(error.contains("Wi-Fi password is required"), "{error}");
    assert!(app.job_control.is_none());
    assert_eq!(app.step, Step::Confirm);
}

#[test]
fn write_setup_uses_the_newly_published_document_without_a_render_frame() {
    use tilo_resource::Resource as _;

    let (mut app, mut doc) = seeded_app();
    app.select_target(0);
    app.goto(Step::Confirm);
    assert!(app.profile.wifi_ssid.is_empty());
    Imager::paths()
        .profile
        .wifi_ssid
        .replace("studio")
        .apply_local(&mut doc)
        .unwrap();
    assert!(
        app.profile.wifi_ssid.is_empty(),
        "the previous projection is still cached"
    );
    app.present_state(&Imager::read(&doc).expect("the updated resource"));
    let error = app
        .begin_write(&app.target().unwrap().dev_path(), "sdb")
        .expect_err("the newly published network requires its password");
    assert!(error.contains("Wi-Fi password is required"), "{error}");
    assert_eq!(app.profile.wifi_ssid, "studio");
    assert!(app.job_control.is_none());
    assert_eq!(app.step, Step::Confirm);
}

#[test]
fn official_installable_request_preserves_real_handoff_and_disabled_writer_check() {
    let (mut app, _doc) = seeded_app();
    assert!(app.customizable(), "selected source is catalog-matched");
    assert!(app.offers_write_setup(), "official Confirm offers setup");
    app.select_target(0);
    app.goto(Step::Confirm);
    app.profile.wifi_ssid = "studio".into();
    app.wifi_psk = "correct-horse".into();
    let mut env = tilo_test_env::ScopedEnv::new();
    let local_root = tempfile::tempdir().unwrap();
    let staging = local_root.path().join("staging");
    std::fs::create_dir(&staging).unwrap();
    env.set("TMPDIR", &staging);
    env.set("TEMP", &staging);
    env.set("TMP", &staging);
    let identity_dir = local_root.path().join("identity");
    let system = tilo_device_identity::IdentityPaths::under(local_root.path().join("system"));
    let local = crate::handoff::LocalIdentity {
        identity_dir: &identity_dir,
        system: &system,
    };
    let staged = crate::handoff::stage_setup_handoff(&app.profile, &app.wifi_psk, &local)
        .expect("official install profile must allow real Wi-Fi handoff")
        .expect("a selected network produces actual staged data");
    let network = staged
        .path()
        .join("etc/NetworkManager/system-connections/tilo-setup.nmconnection");
    let contents = std::fs::read_to_string(&network).unwrap();
    assert!(contents.contains("\nssid=studio\n"));
    assert!(contents.contains("\npsk=correct-horse\n"));
    let request = app
        .write_request(
            app.source.clone().unwrap(),
            app.target().unwrap().clone(),
            Some(staged.root.clone()),
        )
        .unwrap();
    assert!(request.source.installable);
    assert_eq!(request.profile.wifi_ssid, "studio");
    assert_eq!(request.handoff_stage.as_deref(), Some(staged.path()));
    let (start, receiver, control) =
        crate::write::spawn_job(request, "sdb", crate::write::WriteBackend::Disabled);
    assert_eq!(start, crate::write::JobStart::WriteDisabled);
    assert!(receiver.is_none());
    assert!(control.is_none());
    assert!(app.job_control.is_none());
    assert_eq!(app.step, Step::Confirm);
    assert_eq!(std::fs::read_to_string(network).unwrap(), contents);
    std::fs::remove_dir_all(staged.root).unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn release_download_is_local_visible_and_cancellable() {
    {
        let (mut app, _doc) = seeded_app();
        app.release_download = Some(ReleaseDownload {
            index: 0,
            control: crate::release::DownloadControl::default(),
        });

        let release_names = [
            "Tilo 1.0, Preparing",
            "Tilo 1.1-rc2, Generic x86-64, testing channel, downloads first",
        ]
        .map(String::from);
        let mut view = harness(app, AutoCommit::new(), (1180, 820));
        let release_nodes = view
            .controls()
            .into_iter()
            .filter(|control| {
                control
                    .name
                    .as_ref()
                    .is_some_and(|name| release_names.contains(name))
            })
            .collect::<Vec<_>>();
        assert_eq!(
            release_nodes.len(),
            release_names.len(),
            "every offered release has an actual accessible row"
        );
        assert_eq!(
            release_nodes
                .iter()
                .filter(|control| !control.disabled)
                .count(),
            1,
            "only the active release remains enabled during its download"
        );
        let mut app = view.root_mut::<ImagerApp>();
        assert!(
            app.published_rows()
                .iter()
                .any(|row| row.id() == "imager.download"),
            "the download is published with its cancel"
        );

        app.cancel_release_download().unwrap();
        assert!(
            app.release_download
                .as_ref()
                .is_some_and(|download| download.control.is_cancelled())
        );
    }

    {
        let mut host = TestHost::capture(Some("source"));
        host.with_widget::<ImagerApp, _>(|app| {
            app.releases = crate::model::sample_releases();
            app.release_download = Some(ReleaseDownload {
                index: 0,
                control: crate::release::DownloadControl::default(),
            });
        })
        .unwrap();
        host.settle();
        assert!(
            host.harness()
                .a11y_nodes()
                .iter()
                .any(|(_, n)| n.label() == Some("Cancel download"))
        );
    }

    {
        let (mut app, _doc) = seeded_app();
        let progress = JobProgress {
            phase: "downloading".into(),
            completed: Some(25),
            total: Some(100),
        };
        assert!(ImagerApp::download_status(&progress).contains("25%"));

        app.release_download = Some(ReleaseDownload {
            index: 0,
            control: crate::release::DownloadControl::default(),
        });
        app.finish_release_download(Err("Tilo installer download cancelled.".into()));
        assert!(app.release_download.is_none());
        assert!(app.status.contains("cancelled"));
        assert!(
            !app.published_rows()
                .iter()
                .any(|row| row.id() == "imager.download")
        );
    }
}

#[test]
fn owner_ssh_key_can_be_typed_before_the_complete_key_is_validated() {
    let mut host = TestHost::capture(Some("confirm"));
    host.harness()
        .perform_named(
            "imager.write-setup.ssh-key",
            &ui::a11y::Action::SetValue("ssh-".into()),
        )
        .unwrap();
    assert_eq!(
        host.with_widget::<ImagerApp, _>(|app| app.ssh_public_key.with(Clone::clone))
            .unwrap(),
        "ssh-"
    );
    host.with_widget::<ImagerApp, _>(|app| {
        let error = app
            .begin_write(&app.target().unwrap().dev_path(), "sdb")
            .expect_err("an incomplete key must refuse the write");
        assert!(error.contains("public key"), "unexpected refusal: {error}");
        assert!(app.job_control.is_none());
    })
    .expect("the hosted wizard");
}

#[test]
#[cfg(not(feature = "host-media-writer"))]
fn clicking_through_the_wizard_refuses_a_fake_write_when_backend_is_disabled() {
    let mut host = TestHost::capture(Some("source"));
    host.harness().click_named("imager.next").unwrap();
    assert_eq!(
        host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
        Step::Drive
    );
    host.harness().click_named("imager.drive.sdb").unwrap();
    assert_eq!(
        host.with_widget::<ImagerApp, _>(|a| a.selected).unwrap(),
        Some(0)
    );
    host.harness().click_named("imager.next").unwrap();
    assert_eq!(
        host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
        Step::Customize
    );
    host.harness().click_named("imager.next").unwrap();
    assert_eq!(
        host.with_widget::<ImagerApp, _>(|a| a.step).unwrap(),
        Step::Confirm
    );
    host.harness().click_named("imager.write").unwrap();
    host.with_widget::<ImagerApp, _>(|a| {
        assert_eq!(a.step, Step::Confirm);
        assert_eq!(a.progress.phase, JobPhase::Idle);
        assert!(a.job_control.is_none());
    })
    .unwrap();
}

#[test]
fn erase_and_write_presses_the_drives_name_confirmed_write() {
    let (mut app, _doc) = seeded_app();
    app.select_target(0); // sdb
    app.goto(Step::Confirm);
    let row = app.target_row(app.target().unwrap()).into_resource();
    let write = row.actions.iter().find(|spec| spec.id == "write").unwrap();
    assert_eq!(
        write.contract.confirmation,
        tilo_actions::ConfirmationPolicy::Phrase {
            phrase: "sdb".into()
        }
    );
    app.advance();
    assert_eq!(app.step, Step::Confirm);
    assert_eq!(app.progress.phase, JobPhase::Idle);
    assert!(app.job_control.is_none());
}

fn chosen_handle(path: &std::path::Path) -> ui::portal::ChosenHandle {
    let file = std::fs::File::open(path).unwrap();
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    ui::portal::ChosenHandle::new(file, path.to_path_buf(), name)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn source_and_target_selection_accept_only_inspected_external_media() {
    {
        let (mut app, _doc) = seeded_app();
        let f = tempfile::Builder::new().suffix(".iso").tempfile().unwrap();
        std::fs::write(f.path(), b"tilo payload for the imager test").unwrap();
        app.take_chosen_image(Some(chosen_handle(f.path())))
            .unwrap();
        assert_eq!(app.source.as_ref().unwrap().kind, SourceKind::IsoFile);
        app.take_chosen_image(None).unwrap();
        assert!(app.status.contains("cancelled"));
    }

    {
        let (app, _doc) = seeded_app();
        assert_eq!(app.visible_targets().len(), 1);
        assert_eq!(app.removable_count(), 1);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);

        app.apply_drive_inventory(Vec::new());

        assert!(app.targets.is_empty());
        assert_eq!(app.selected, None);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        let selected = app.target().unwrap().clone();
        let mut reordered = crate::model::sample_targets();
        reordered.swap(0, 1);

        app.apply_drive_inventory(reordered);

        assert_eq!(app.selected, Some(1));
        assert_eq!(
            app.target().and_then(|drive| drive.stable_id.as_deref()),
            selected.stable_id.as_deref()
        );

        let mut replacement = selected;
        replacement.stable_id = Some("different-usb-stick".into());
        app.apply_drive_inventory(vec![replacement]);

        assert_eq!(
            app.selected, None,
            "a reused OS device slot is not the reviewed drive"
        );

        let mut unidentified_before = crate::model::sample_targets()[0].clone();
        unidentified_before.stable_id = None;
        app.targets = vec![unidentified_before.clone()];
        app.selected = Some(0);
        app.apply_drive_inventory(vec![unidentified_before]);
        assert_eq!(
            app.selected, None,
            "a drive without stable identity must be chosen again after rescan"
        );
    }

    {
        let mut app = ImagerApp::new();
        let mut targets = crate::model::sample_targets();
        targets[1].is_system = true; // the internal NVMe is the system disk
        app.targets = targets.clone();
        let doc = document(&Imager {
            source: Some(crate::model::sample_source()),
            ..Imager::default()
        });
        present(&mut app, &doc);

        app.select_target(1);
        assert_eq!(app.selected, None, "the system disk is never selectable");
        assert!(app.status.to_lowercase().contains("system disk"));
    }
}

#[test]
fn profile_panel_edits_persist_through_the_real_event_path() {
    {
        let (mut app, _doc) = seeded_app();
        app.select_target(1);
        app.export_config();
        let text = app.export_dialog.text();
        assert!(text.contains("\"schema\": 1"));
        assert!(text.contains("\"hostname\""));
        assert!(!text.contains("password"));
        assert!(app.export_dialog.is_open());
    }
}

#[test]
fn capture_progress_owns_worker_until_actual_host_teardown() {
    let mut host = TestHost::capture_at(Some("progress"), (1180, 820));
    let control = host
        .surface_mut::<ImagerApp>()
        .job_control
        .as_ref()
        .expect("progress capture owns its worker")
        .clone();
    assert!(!control.is_cancelled());
    assert!(!control.observed_stopped());
    host.harness().close_components();
    assert!(control.is_cancelled());
    assert!(control.observed_stopped(), "actual worker must be joined");
    assert!(!control.observed_failure());
    assert!(
        host.harness()
            .component_close_reports()
            .iter()
            .all(|report| {
                report.view_error.is_none()
                    && report.resource_error.is_none()
                    && report.teardown_error.is_none()
            }),
        "a component closed with an error (view, record, teardown): {:?}",
        host.harness()
            .component_close_reports()
            .iter()
            .map(|report| (
                &report.view_error,
                &report.resource_error,
                &report.teardown_error
            ))
            .collect::<Vec<_>>()
    );

    let unbound = JobControl::new();
    unbound.cancel();
    assert!(unbound.wait_stopped().is_err());
    assert!(!unbound.observed_stopped());
}

fn held_semantic_job_fixture(cancel: bool, panic_after_success: bool) {
    use std::{
        sync::mpsc,
        time::{Duration, Instant},
    };
    let mut host = crate::app::sample_host();
    let (release, held) = mpsc::sync_channel(1);
    let (ready, started) = mpsc::sync_channel(1);
    let (send, events) = mpsc::channel();
    let control = JobControl::owned_fixture(std::thread::spawn(move || {
        send.send(Progress {
            phase: JobPhase::Success,
            write_frac: 1.0,
            verify_frac: 1.0,
            ..Progress::default()
        })
        .unwrap();
        ready.send(()).unwrap();
        held.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert!(!panic_after_success, "fixture panic after queued success");
    }));
    started.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    let jobs = {
        let app = host.surface_mut::<ImagerApp>();
        app.goto(Step::Write);
        app.job_control = Some(control.clone());
        let port = app.port.clone();
        ImagerApp::start_write_job(&port, events, control.clone(), 4096);
        port
    };
    let deadline = tilo_test_env::hang_deadline();
    loop {
        host.settle();
        if jobs
            .job(WRITE_JOB)
            .is_some_and(|progress| progress.phase == JobPhase::Success.key())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal progress was not delivered"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        jobs.job(WRITE_JOB).is_some(),
        "semantic job completed from progress alone"
    );
    assert!(!control.observed_stopped());
    assert!(host.surface_mut::<ImagerApp>().job_control.is_some());
    assert_eq!(host.surface_mut::<ImagerApp>().step, Step::Write);
    if cancel {
        control.cancel();
    }
    assert!(
        jobs.job(WRITE_JOB).is_some(),
        "cancellation request was treated as native completion"
    );
    release.send(()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while jobs.job(WRITE_JOB).is_some() {
        host.settle();
        assert!(
            Instant::now() < deadline,
            "semantic job did not observe writer exit"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(control.observed_stopped());
    assert_eq!(control.observed_failure(), panic_after_success);
    assert_eq!(
        control.wait_stopped().is_err(),
        panic_after_success,
        "async native join failure was lost"
    );
    let app = host.surface_mut::<ImagerApp>();
    assert!(app.job_control.is_none());
    assert_eq!(
        app.progress.phase,
        if cancel || panic_after_success {
            JobPhase::Failed
        } else {
            JobPhase::Success
        }
    );
    assert_eq!(app.step, Step::Done);
    if panic_after_success {
        assert_eq!(
            app.progress.error.as_deref(),
            Some(crate::setup::strings::t("imager-write-worker-stopped").as_str())
        );
    }
}

#[test]
fn semantic_write_job_stays_running_after_terminal_event_until_owned_exit() {
    held_semantic_job_fixture(false, false);
    held_semantic_job_fixture(true, false);
}

#[test]
fn semantic_write_job_reports_native_panic_after_queued_success() {
    held_semantic_job_fixture(false, true);
}

#[test]
fn terminal_progress_defers_close_until_owned_writer_exits() {
    use std::sync::mpsc;
    use ui::component_lifecycle::ComponentLifecycle;
    let (release, held) = mpsc::sync_channel(1);
    let control = JobControl::owned_fixture(std::thread::spawn(move || {
        held.recv_timeout(tilo_test_env::hang_bound()).unwrap();
    }));
    let (mut app, _) = seeded_app();
    app.goto(Step::Write);
    app.job_control = Some(control.clone());
    app.finish_write(Some(Progress {
        phase: JobPhase::Success,
        ..Progress::default()
    }));
    assert_eq!(
        app.step,
        Step::Write,
        "terminal progress released the writer"
    );
    assert!(matches!(
        app.on_window_close_requested(),
        ui::widget::WindowCloseAdmission::Defer
    ));
    assert!(control.is_cancelled());
    assert!(!app.take_window_close_intent());
    assert!(app.writer_refusal().is_err());
    release.send(()).unwrap();
    control.wait_stopped().unwrap();
    app.drain_job();
    assert!(app.job_control.is_none());
    assert_eq!(
        app.progress.phase,
        JobPhase::Failed,
        "cancelled close raced into success"
    );
    assert!(!app.close_work_active());
    let doc = document(&Imager::default());
    present(&mut app, &doc);
    assert!(app.take_window_close_intent());
    present(&mut app, &doc);
    assert!(!app.take_window_close_intent());
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each merged phase and its fresh fixture visible"
)]
fn write_completion_failure_and_cancel_return_to_a_truthful_state() {
    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        app.progress = Progress {
            phase: JobPhase::Success,
            write_frac: 1.0,
            verify_frac: 1.0,
            rate_bps: 0,
            error: None,
        };
        app.write_another();
        assert_eq!(app.step, Step::Source);
        assert_eq!(app.progress.phase, JobPhase::Idle);
    }

    {
        let (mut app, _doc) = seeded_app();
        app.select_target(0);
        app.goto(Step::Write);
        app.progress = Progress {
            phase: JobPhase::Writing,
            write_frac: 0.4,
            verify_frac: 0.0,
            rate_bps: 0,
            error: None,
        };
        app.job_control = Some(JobControl::finished_fixture());
        app.finish_write(Some(Progress {
            phase: JobPhase::Failed,
            write_frac: 0.4,
            verify_frac: 0.0,
            rate_bps: 0,
            error: Some("Could not write the drive: the selected drive was disconnected.".into()),
        }));
        assert_eq!(app.step, Step::Done);
        assert_eq!(app.progress.phase, JobPhase::Failed);
        assert!(
            app.status.contains("disconnected"),
            "the privileged writer's actionable cause reaches the UI"
        );
        let h = harness(app, AutoCommit::new(), (1180, 820));
        assert!(
            h.inspection_json().unwrap().contains("disconnected"),
            "the writer's actionable cause is exposed by the retained result"
        );
    }

    {
        let (mut app, _doc) = seeded_app();
        let control = JobControl::new();
        app.job_control = Some(control.clone());
        app.goto(Step::Write);

        app.cancel_write().unwrap();

        assert!(control.is_cancelled());
        assert!(app.status.contains("Keep the drive connected"));
        let mut h = harness(app, AutoCommit::new(), (1180, 820));
        h.expect(&ui::headless::SemanticExpectation::named("imager.cancel-write").enabled(false))
            .unwrap();
    }

    {
        let (mut app, _doc) = seeded_app();
        let start = ui::time::now();
        let report = |phase: &str, completed| JobProgress {
            phase: phase.into(),
            completed: Some(completed),
            total: Some(1_000),
        };
        app.follow_write(&report("writing", 100), start);
        app.follow_write(
            &report("writing", 300),
            start + std::time::Duration::from_secs(2),
        );
        assert_eq!(app.progress.phase, JobPhase::Writing);
        assert!((app.progress.write_frac - 0.3).abs() < 1e-9);
        assert_eq!(app.progress.rate_bps, 100, "200 bytes in two seconds");
        app.follow_write(
            &report("verifying", 500),
            start + std::time::Duration::from_secs(3),
        );
        assert_eq!(app.progress.phase, JobPhase::Verifying);
        assert!((app.progress.write_frac - 1.0).abs() < 1e-9);
        assert!((app.progress.verify_frac - 0.5).abs() < 1e-9);

        app.job_control = Some(JobControl::finished_fixture());
        app.finish_write(None);
        assert_eq!(app.progress.phase, JobPhase::Failed);
        assert!(app.status.contains("stopped unexpectedly"));
        assert!(app.job_control.is_none());
    }

    {
        let (mut app, _doc) = seeded_app();
        let control = JobControl::finished_fixture();
        control.cancel();
        app.job_control = Some(control);
        app.progress.phase = JobPhase::Verifying;
        app.goto(Step::Write);
        app.finish_write(Some(Progress {
            phase: JobPhase::Success,
            write_frac: 1.0,
            verify_frac: 1.0,
            rate_bps: 0,
            error: None,
        }));

        assert_eq!(app.progress.phase, JobPhase::Failed);
        assert!(app.progress.error.as_deref().is_some_and(|message| {
            message.contains("incomplete") && message.contains("review and retry")
        }));
        assert_eq!(app.step, Step::Done);
    }
}
