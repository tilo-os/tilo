use tilo_automerge::ApplyIncoming;

include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    doc.put(ROOT, "source", false).unwrap();
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    let text = doc
        .put_object(ROOT, "source", automerge::ObjType::Text)
        .unwrap();
    doc.splice_text(&text, 0, 0, "= Selected source\nHistorical paragraph.")
        .unwrap();
    let selected = doc.get_heads()[0];
    ui::doc::splice_text_diffed(&mut doc, &text, "= Live source\nCurrent paragraph.");
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Selected source".into())
}
fn verify_restored(doc: &AutoCommit) {
    let text = doc.get(ROOT, "source").unwrap().unwrap().1;
    assert_eq!(
        doc.text(&text).unwrap(),
        "= Selected source\nHistorical paragraph."
    );
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    Box::new(crate::app::TypstApp::from_library(None))
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "merged scenario: each former test is a labelled case block with its own fixture"
)]
fn library_restore_and_compilation_history_are_exact() {
    // Case `absent_source_refuses_restore_but_empty_source_is_exact`.
    {
        let mut doc = AutoCommit::new();
        doc.put(ROOT, "view_only", "current").unwrap();
        let absent = doc.get_heads()[0];
        let source = doc
            .put_object(ROOT, "source", automerge::ObjType::Text)
            .unwrap();
        let empty = doc.get_heads()[0];
        doc.splice_text(&source, 0, 0, "Current source").unwrap();
        let heads = doc.get_heads();
        let p = preview(&doc, absent, PreviewSide::After);
        assert!(p.accessibility().description.contains("No source recorded"));
        assert!(
            prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &[absent])
                .is_err()
        );
        assert_eq!(doc.base_heads(), heads);
        assert_eq!(doc.pending_ops(), 0);
        assert_eq!(doc.text(&source).unwrap(), "Current source");
        let mut driver = managed_driver(doc.clone(), absent);
        let undo_before = format!("{:?}", driver.state_mut().undo_status(None));
        let live_before = driver.state_mut().doc.base_heads();
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        assert_eq!(driver.state_mut().doc.base_heads(), live_before);
        assert_eq!(driver.state_mut().doc.pending_ops(), 0);
        assert_eq!(
            format!("{:?}", driver.state_mut().undo_status(None)),
            undo_before
        );
        assert_eq!(
            driver.state_mut().doc.text(&source).unwrap(),
            "Current source"
        );
        driver.key("Escape");
        select_version(&mut driver, empty);
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        let restored = driver
            .state_mut()
            .doc
            .get(ROOT, "source")
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(driver.state_mut().doc.text(&restored).unwrap(), "");
        assert!(driver.state_mut().can_undo_as(None));

        select_version(&mut driver, heads[0]);
        driver.click_named("history-restore").unwrap();
        driver.key("Enter");
        let restored = driver
            .state_mut()
            .doc
            .get(ROOT, "source")
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(
            driver.state_mut().doc.text(&restored).unwrap(),
            "Current source"
        );
        let plan =
            prepare_content_restore(&doc, &crate::framework_history::SCHEMA, &heads, &[empty])
                .unwrap();
        apply_prepared_content_restore(&mut doc, &plan).unwrap();
        doc.get_heads();
        let mut root = test_root();
        root.sync(&mut doc, &ROOT);
        let text = doc.get(ROOT, "source").unwrap().unwrap().1;
        assert_eq!(doc.text(&text).unwrap(), "");
    }
    // Case `invalid_source_has_diagnostics_and_multipage_compilation_is_navigable`.
    {
        for text in [
            "= Invalid source\n#let x = (",
            "= Page one\n#pagebreak()\n= Page two",
        ] {
            let mut doc = AutoCommit::new();
            let obj = doc
                .put_object(ROOT, "source", automerge::ObjType::Text)
                .unwrap();
            doc.splice_text(&obj, 0, 0, text).unwrap();
            let selected = doc.get_heads()[0];
            let mut p = preview(&doc, selected, PreviewSide::After);
            assert!(p.accessibility().description.contains(text));
            let pages = p.accessibility().pages;
            if text.contains("Invalid") {
                assert!(pages.iter().any(|(id, _)| id == "diagnostics"));
                assert!(!pages.iter().any(|(id, _)| id.starts_with("page:")));
                assert!(p.navigate(PreviewNavigation::SelectPage("diagnostics".into())));
                assert!(!p.accessibility().description.is_empty());
            } else {
                assert!(p.navigate(PreviewNavigation::SelectPage("page:1".into())));
                assert!(p.accessibility().description.contains("Page two"));
            }
            let mut tr = ui::TextRenderer::new();
            for width in [360., 1000.] {
                p.layout(
                    vello::kurbo::Rect::new(0., 0., width, 300.),
                    &mut tr,
                    &ui::Theme::dark(),
                )
                .unwrap();
                p.draw(
                    &mut vello::Scene::new(),
                    vello::kurbo::Affine::IDENTITY,
                    &mut tr,
                    &ui::Theme::dark(),
                );
            }
        }
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated write/read subprocess custody test"
)]
fn production_library_persisted_restore_and_fresh_reopen() {
    if let Ok(stage) = std::env::var("TILO_SLICE5_TYPST_STAGE") {
        let (mut state, mut saver) = ui::persist::try_open("so.tilo.typst-editor").unwrap();
        if stage == "write" {
            let (mut doc, selected, _) = fixture();
            state.doc.merge_incoming(&mut doc).unwrap();
            state.doc.get_heads();
            saver.request(&mut state.doc);
            saver.try_flush().unwrap();
            (state, saver) = ui::persist::try_open("so.tilo.typst-editor").unwrap();
            let before = state.doc.base_heads();
            let mut root = crate::app_root();
            root.sync(&mut state.doc, &ROOT);
            assert_eq!(
                state.doc.get_heads(),
                before,
                "production startup must retain saved source"
            );
            let def =
                ui::history_mode::capability::document_history_definition(root.as_ref()).unwrap();
            let reg = HistoryRegistration::new(
                def,
                vec![HistorySourceRegistration::HostDocument {
                    scope: "source",
                    resource: HistoryResource::new(
                        "temporary",
                        "production",
                        def.app_id,
                        ui::RecordId::for_tab(55),
                    ),
                }],
            )
            .unwrap();
            let mut driver = ui::headless::Driver::from_managed_root(
                state,
                ManagedRoot::new(root, reg),
                (900, 700),
            )
            .unwrap();
            driver.click_named("history-open").unwrap();
            select_version(&mut driver, selected);
            driver.click_named("history-restore").unwrap();
            driver.key("Enter");
            verify_restored(&driver.state_mut().doc);
            driver.click_named("history-back-live").unwrap_or_else(|e| {
                panic!(
                    "{e}; targets={:?}; a11y={:?}",
                    driver.targets(),
                    driver.a11y_nodes()
                )
            });
            verify_restored(&driver.state_mut().doc);
            let source = driver
                .state_mut()
                .doc
                .get(ROOT, "source")
                .unwrap()
                .unwrap()
                .1;
            ui::doc::splice_text_diffed(&mut driver.state_mut().doc, &source, "");
            driver.state_mut().doc.get_heads();
            let empty = driver.state_mut().doc.base_heads();
            ui::doc::splice_text_diffed(&mut driver.state_mut().doc, &source, "Later nonempty");
            driver.state_mut().doc.get_heads();
            let heads = driver.state_mut().doc.base_heads();
            let plan = prepare_content_restore(
                &driver.state_mut().doc,
                &crate::framework_history::SCHEMA,
                &heads,
                &empty,
            )
            .unwrap();
            apply_prepared_content_restore(&mut driver.state_mut().doc, &plan).unwrap();
            driver.state_mut().doc.get_heads();
            driver.resize((950, 700)).unwrap();
            let restored_source = driver
                .state_mut()
                .doc
                .get(ROOT, "source")
                .unwrap()
                .unwrap()
                .1;
            assert_eq!(driver.state_mut().doc.text(&restored_source).unwrap(), "");
            saver.request(&mut driver.state_mut().doc);
            saver.try_flush().unwrap();
            std::fs::write(
                saver.document_path().with_extension("selected"),
                selected.to_string(),
            )
            .unwrap();
        } else {
            let heads = state.doc.base_heads();
            let mut root = crate::app_root();
            root.sync(&mut state.doc, &ROOT);
            assert_eq!(state.doc.get_heads(), heads);
            let source = state.doc.get(ROOT, "source").unwrap().unwrap().1;
            assert_eq!(state.doc.text(&source).unwrap(), "");
            let selected =
                std::fs::read_to_string(saver.document_path().with_extension("selected"))
                    .unwrap()
                    .parse()
                    .unwrap();
            assert!(state.doc.get_change_by_hash(&selected).is_some());
        }
        return;
    }
    let dir = std::env::temp_dir().join(format!(
        "tilo-slice5-typst-production-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("unrelated.typ"),
        "= External library must not replace source",
    )
    .unwrap();
    for stage in ["write", "read"] {
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "framework_history_tests::production_library_persisted_restore_and_fresh_reopen",
                "--nocapture",
            ])
            .env("TILO_SLICE5_TYPST_STAGE", stage)
            .env("TILO_TYPST_LIBRARY_DIR", &dir)
            .env("XDG_DATA_HOME", &dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{stage}: {} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("test result: ok. 1 passed"),
            "the child ran no test: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("unrelated.typ")).unwrap(),
        "= External library must not replace source"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
