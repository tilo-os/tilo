thread_local! {
    static NEXT_JOURNEY_IMPORT: std::cell::RefCell<Option<ImportApp>> = const { std::cell::RefCell::new(None) };
    static JOURNEY_URIS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}
struct JourneyImportFlow;
impl ui::framework::ResourceApp for JourneyImportFlow {
    type Resource = Imager;
    const ID: &'static str = "so.tilo.imager";
    const TITLE: &'static str = "Disk Imager";
    fn view(cx: &mut ViewCx<Imager>) -> Element {
        cx.surface(
            NEXT_JOURNEY_IMPORT
                .with(|slot| slot.borrow_mut().take().expect("private fixture imports")),
        )
    }
}
fn mount_journey_import(app: ImportApp) -> ui::framework::testing::TestApp<JourneyImportFlow> {
    NEXT_JOURNEY_IMPORT.with(|slot| *slot.borrow_mut() = Some(app));
    ui::framework::testing::TestApp::<JourneyImportFlow>::new().unwrap()
}
fn journey_open(uri: &str) -> ui::portal::LaunchChannel {
    JOURNEY_URIS.with(|uris| uris.borrow_mut().push(uri.into()));
    let (tx, rx) = mpsc::channel();
    tx.send(ui::portal::LaunchOutcome::Opened).unwrap();
    rx
}
#[test]
fn journey_copy_later_restart_keeps_identity_gate_and_completion_destinations() {
    JOURNEY_URIS.with(|uris| uris.borrow_mut().clear());
    let (temp, mut app, source, destination) = fixture();
    settle_index(&mut app);
    fs::write(source.join("kept.txt"), b"source survives").unwrap();
    let later = temp.path().join("later");
    fs::create_dir(&later).unwrap();
    fs::write(later.join("later.txt"), b"still local").unwrap();
    let mut choice = continuation_choice(&later);
    choice.mode = super::super::model::FolderMode::Later;
    let mut c = Continuation::capture(&[continuation_choice(&source), choice]).unwrap();
    c.resolve(0, &destination, true).unwrap();
    c.save(app.store.as_ref().unwrap()).unwrap();
    app.continuation = Some(c);
    app.navigation.opener = journey_open;
    assert!(app.admit(ImportEffect::OpenAccounts).is_err());
    assert!(app.admit(ImportEffect::OpenStore).is_err());
    JOURNEY_URIS.with(|uris| assert!(uris.borrow().is_empty()));
    app.identity_checker = identity_ready;
    app.request_identity_gate(ImportCommand::CheckIdentity)
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while app.gate_request.is_some() {
        app.poll_background();
        assert!(ui::time::now() < deadline);
        std::thread::yield_now();
    }
    assert!(app.identity_ready);
    app.prepare_continued_folder(0).unwrap();
    settle_index(&mut app);
    app.start_copy(0).unwrap();
    wait_terminal(&mut app);
    assert_eq!(
        fs::read(destination.join("source/kept.txt")).unwrap(),
        b"source survives"
    );
    assert_eq!(
        fs::read(source.join("kept.txt")).unwrap(),
        b"source survives"
    );
    assert!(!destination.join("later").exists());
    let ownership_released = super::super::migration_store::test_support::ownership_release(
        app.store.as_deref().unwrap(),
    );
    drop(app);
    tilo_test_env::recv(
        "the previous import store's ownership release",
        &ownership_released,
    );
    let store = MigrationStore::open_at(&temp.path().join("state")).unwrap();
    let mut reopened = ImportApp::with_store(store).unwrap();
    settle_index(&mut reopened);
    assert_eq!(
        reopened.continuation.as_ref().unwrap().folders[1].mode,
        ChoiceMode::Later
    );
    assert_eq!(reopened.operations.len(), 1);
    assert!(reopened.admit(ImportEffect::OpenStore).is_err());
    reopened.identity_checker = identity_ready;
    reopened
        .request_identity_gate(ImportCommand::CheckIdentity)
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    while reopened.gate_request.is_some() {
        reopened.poll_background();
        assert!(ui::time::now() < deadline);
        std::thread::yield_now();
    }
    reopened.navigation.opener = journey_open;
    let mut mounted = mount_journey_import(reopened);
    for id in ["import.journey.accounts", "import.journey.store"] {
        mounted
            .harness()
            .perform_named(id, &ui::a11y::Action::Activate)
            .unwrap();
        mounted.settle();
        let status = mounted
            .effect(&ImportEffect::ReadStatus)
            .unwrap()
            .value
            .unwrap();
        assert_eq!(status["status_is_error"], false);
        assert_eq!(
            status["operations"].as_array().unwrap().len(),
            1,
            "navigation must not duplicate copies"
        );
    }
    JOURNEY_URIS.with(|uris| {
        assert_eq!(
            *uris.borrow(),
            vec!["settings://accounts", "tilo-store:catalogue"]
        )
    });
}

#[test]
fn journey_portal_refusal_is_observed_and_can_be_retried() {
    let (_temp, mut app, source, _destination) = fixture();
    settle_index(&mut app);
    app.continuation = Some(Continuation::capture(&[continuation_choice(&source)]).unwrap());
    app.identity_ready = true;
    app.navigation.opener = |_| {
        let (tx, rx) = mpsc::channel();
        tx.send(ui::portal::LaunchOutcome::Failed(
            "portal refused the request".into(),
        ))
        .unwrap();
        rx
    };
    assert!(matches!(
        app.admit(ImportEffect::OpenAccounts).unwrap(),
        Admitted::Done(_)
    ));
    assert!(!app.import_projection().journey_available);
    assert!(app.admit(ImportEffect::OpenStore).is_err());
    app.poll_background();
    assert!(app.status_is_error);
    assert_eq!(app.status, "portal refused the request");
    assert!(app.import_projection().journey_available);
}

#[test]
fn journey_import_screenshots_follow_trusted_fixture_readiness_across_themes() {
    for (name, theme) in [
        ("dark", Theme::dark()),
        ("light", Theme::light()),
        ("high-contrast", Theme::high_contrast()),
    ] {
        let mut app = mount_journey_import(ImportApp::for_shot("import-first-login"));
        app.harness().set_theme(theme);
        app.harness()
            .expect(
                &ui::headless::SemanticExpectation::named("import.journey.accounts").enabled(true),
            )
            .unwrap();
        app.harness()
            .expect(&ui::headless::SemanticExpectation::named("import.journey.store").enabled(true))
            .unwrap();
        let image = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../target/onboarding-native-shots/import-{name}.png"
        ));
        fs::create_dir_all(image.parent().unwrap()).unwrap();
        app.harness().render_png(&image);
        assert!(fs::metadata(&image).unwrap().len() > 0);
    }
}
