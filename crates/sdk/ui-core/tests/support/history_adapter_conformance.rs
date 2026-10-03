// Included by app test modules. Required local hooks: fixture(),
// verify_restored(), malformed_fixture(), and the app-supplied test_root().
use automerge::{AutoCommit, ROOT, ReadDoc, transaction::Transactable};
use ui::history_mode::restore::*;
use ui::history_mode::*;

#[test]
fn history_adapter_selects_immutable_content_and_rejects_malformed_input() {
    // Phase `actual_root_declares_framework_history`.
    {
        assert!(
            ui::history_mode::capability::document_history_definition(test_root().as_ref())
                .is_some(),
            "production root must declare its domain History"
        );
    }
    // Phase `selected_content_is_immutable_navigable_and_restorable`.
    {
        let (mut doc, selected, expected) = fixture();
        let heads = doc.get_heads();
        let mut p = preview(&doc, selected, PreviewSide::After);
        assert!(
            p.accessibility().description.contains(&expected),
            "{:?}",
            p.accessibility()
        );
        let before = preview(&doc, selected, PreviewSide::Before);
        assert_ne!(
            before.accessibility().description,
            p.accessibility().description
        );
        assert!(!p.accessibility().pages.is_empty());
        let pages = p.accessibility().pages;
        let mut tr = ui::TextRenderer::new();
        for width in [360.0, 1000.0] {
            for (page, _) in &pages {
                assert!(p.navigate(PreviewNavigation::SelectPage(page.clone())));
                p.layout(
                    vello::kurbo::Rect::new(0., 0., width, 600.),
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
                assert!(p.navigate(PreviewNavigation::Scroll { dx: 0., dy: -80. }));
            }
        }
        assert!(!p.navigate(PreviewNavigation::SelectPage("unknown".into())));
        assert_eq!(doc.get_heads(), heads);
        assert_eq!(doc.pending_ops(), 0);
        let schema = definition().scopes[0].content_schema.unwrap();
        let restore = prepare_content_restore(&doc, schema, &heads, &[selected]).unwrap();
        assert_eq!(
            apply_prepared_content_restore(&mut doc, &restore).unwrap(),
            RestoreOutcome::Changed
        );
        verify_restored(&doc);
        assert_eq!(
            doc.get(ROOT, "view_only").unwrap().unwrap().0.as_str(),
            Some("current")
        );
        let restored = doc.get_heads().pop().unwrap();
        assert!(
            preview(&doc, restored, PreviewSide::After)
                .accessibility()
                .description
                .contains(&expected)
        );
    }
    // Phase `malformed_content_is_rejected_without_writes`.
    {
        let (mut doc, selected, _) = fixture();
        malformed_fixture(&mut doc);
        let heads = doc.get_heads();
        assert!(
            prepare_content_restore(
                &doc,
                definition().scopes[0].content_schema.unwrap(),
                &heads,
                &[selected]
            )
            .is_err()
        );
        assert_eq!(doc.base_heads(), heads);
        assert_eq!(doc.pending_ops(), 0);
    }
}

fn definition() -> &'static HistoryDefinition {
    ui::history_mode::capability::document_history_definition(test_root().as_ref())
        .expect("production domain History")
}

fn preview(
    doc: &AutoCommit,
    hash: automerge::ChangeHash,
    side: PreviewSide,
) -> Box<dyn HistoryPreview> {
    let def = definition();
    let mut controller = HistoryController::new(
        HistoryResource::new(
            "temporary",
            "fixture",
            def.app_id,
            ui::RecordId::for_tab(55),
        ),
        doc,
    )
    .unwrap();
    controller
        .select(
            doc,
            &[ui::history_source::entry_id_from_hash(&hash.to_string())],
        )
        .unwrap();
    controller.select_compare_side(side).unwrap();
    let Some(create) = def.scopes[0].presentation.preview_factory() else {
        panic!("domain presentation required")
    };
    create(PreviewInput {
        snapshot: controller.snapshot().unwrap().clone(),
        initial_view: PreviewView::default(),
        assets: FrozenPreviewAssets::default(),
    })
    .unwrap()
}

fn managed_driver(doc: AutoCommit, selected: automerge::ChangeHash) -> ui::headless::Driver {
    let state = ui::State::from_doc(doc);
    let def = definition();
    let registration = HistoryRegistration::new(
        def,
        vec![HistorySourceRegistration::HostDocument {
            scope: def.scopes[0].key,
            resource: HistoryResource::new(
                "temporary",
                "fixture",
                def.app_id,
                ui::RecordId::for_tab(55),
            ),
        }],
    )
    .unwrap();
    let mut driver = ui::headless::Driver::from_managed_root(
        state,
        ManagedRoot::new(test_root(), registration),
        (1000, 750),
    )
    .unwrap();
    driver.click_named("history-open").unwrap();
    select_version(&mut driver, selected);
    driver
}

fn select_version(driver: &mut ui::headless::Driver, selected: automerge::ChangeHash) {
    let target = format!(
        "timeline-entry:{}",
        ui::widget::history_group::hex16(&ui::history_source::entry_id_from_hash(
            &selected.to_string()
        ))
    );
    driver
        .perform_semantic_named(
            "activity_timeline",
            Some(&target),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
}

#[test]
fn history_restore_honours_one_boundary_and_keeps_pending_edits() {
    // Phase `shared_host_restores_one_boundary_and_rejects_revoked_or_stale_review`.
    {
        let (mut doc, selected, expected) = fixture();
        let before = doc.get_heads();
        let live = doc.save();
        let mut d = managed_driver(doc, selected);
        assert!(format!("{:?}", d.a11y_nodes()).contains(&expected));
        d.resize((390, 700)).unwrap();
        assert!(d.targets().contains_key("history-after"));
        d.type_text("do not edit frozen content");
        d.key("Backspace");
        assert_eq!(d.state_mut().doc.base_heads(), before);
        d.click_named("history-before").unwrap();
        d.click_named("history-after").unwrap();
        d.click_named("history-restore").unwrap();
        d.key("Enter");
        verify_restored(&d.state_mut().doc);
        assert_eq!(d.state_mut().doc.get_changes(&before).len(), 1);
        d.click_named("history-back-live").unwrap();
        d.resize((1000, 750)).unwrap();
        verify_restored(&d.state_mut().doc);
        assert!(d.state_mut().undo(None, None).is_some());
        let schema = definition().scopes[0].content_schema.unwrap();
        assert_eq!(
            project_content(&d.state_mut().doc, schema).unwrap(),
            project_content(&AutoCommit::load(&live).unwrap(), schema).unwrap()
        );
        for revoked in [true, false] {
            let (doc, selected, _) = fixture();
            let mut d = managed_driver(doc, selected);
            d.click_named("history-restore").unwrap();
            if revoked {
                d.set_permission_ctx(ui::permissions::PermissionContext::from_capabilities(
                    ui::permissions::Actor::person(None, "revoked"),
                    vec![],
                ));
            } else {
                d.state_mut()
                    .doc
                    .put(ROOT, "remote_operational", "changed")
                    .unwrap();
                d.state_mut().commit_action(None, None);
            }
            let heads = d.state_mut().doc.base_heads();
            d.key("Enter");
            assert_eq!(d.state_mut().doc.base_heads(), heads);
            assert!(d.a11y_node_named("Refresh review").is_some());
        }
    }
    // Phase `pending_live_edits_survive_preview_and_refuse_restore`.
    {
        let (mut doc, selected, _) = fixture();
        let heads = doc.base_heads();
        doc.put(ROOT, "pending_sibling_view", "not committed")
            .unwrap();
        let pending = doc.pending_ops();
        let mut p = preview(&doc, selected, PreviewSide::After);
        p.navigate(PreviewNavigation::NextPage);
        p.navigate(PreviewNavigation::Scroll { dx: 0., dy: -60. });
        assert_eq!(doc.base_heads(), heads);
        assert_eq!(doc.pending_ops(), pending);
        assert_eq!(
            prepare_content_restore(
                &doc,
                definition().scopes[0].content_schema.unwrap(),
                &heads,
                &[selected]
            )
            .unwrap_err(),
            RestoreError::PendingEdits
        );
        assert_eq!(doc.pending_ops(), pending);
        assert_eq!(
            doc.get(ROOT, "pending_sibling_view")
                .unwrap()
                .unwrap()
                .0
                .as_str(),
            Some("not committed")
        );
    }
}

#[test]
fn normal_autosave_fresh_reopen_retains_graph_and_restored_content() {
    let def = definition();
    if let Ok(stage) = std::env::var("TILO_SLICE4_STAGE") {
        let (mut state, mut saver) = ui::persist::try_open(def.app_id).unwrap();
        if stage == "write" {
            let (mut doc, selected, _) = fixture();
            // Included into modules that may import the trait already, so it is named in full.
            tilo_automerge::ApplyIncoming::merge_incoming(&mut state.doc, &mut doc).unwrap();
            state.doc.get_heads();
            saver.request(&mut state.doc);
            saver.try_flush().unwrap();
            let mut d = managed_driver(state.doc, selected);
            d.click_named("history-restore").unwrap();
            d.key("Enter");
            verify_restored(&d.state_mut().doc);
            saver.request(&mut d.state_mut().doc);
            saver.try_flush().unwrap();
            std::fs::write(
                saver.document_path().with_extension("selected"),
                selected.to_string(),
            )
            .unwrap();
        } else {
            let before = state.doc.base_heads();
            test_root().sync(&mut state.doc, &state.root);
            assert_eq!(
                state.doc.base_heads(),
                before,
                "fresh production startup retains the graph"
            );
            assert_eq!(
                state.doc.pending_ops(),
                0,
                "fresh startup does not rewrite restored content"
            );
            verify_restored(&state.doc);
            let selected =
                std::fs::read_to_string(saver.document_path().with_extension("selected"))
                    .unwrap()
                    .parse()
                    .unwrap();
            assert!(state.doc.get_change_by_hash(&selected).is_some());
            let target = state.doc.fork_at(&[selected]).unwrap();
            assert_eq!(
                project_content(&state.doc, def.scopes[0].content_schema.unwrap()).unwrap(),
                project_content(&target, def.scopes[0].content_schema.unwrap()).unwrap()
            );
            assert_eq!(
                state
                    .doc
                    .get(ROOT, "view_only")
                    .unwrap()
                    .unwrap()
                    .0
                    .as_str(),
                Some("current")
            );
        }
        return;
    }
    let dir =
        std::env::temp_dir().join(format!("tilo-slice4-{}-{}", def.app_id, std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for stage in ["write", "read"] {
        let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","framework_history_tests::normal_autosave_fresh_reopen_retains_graph_and_restored_content","--nocapture"]).env("TILO_SLICE4_STAGE",stage).env("XDG_DATA_HOME",&dir).env("XDG_CONFIG_HOME",&dir).env("TILO_PACKAGE_ID",def.app_id).output().unwrap();
        assert!(
            result.status.success(),
            "{stage}: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("test result: ok. 1 passed"),
            "{stage}: the child ran no test: {}",
            String::from_utf8_lossy(&result.stdout)
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

fn project_content(
    doc: &AutoCommit,
    schema: &'static HistoryRestoreSchema,
) -> Result<ContentProjection, RestoreError> {
    let heads = doc.base_heads();
    prepare_content_restore(doc, schema, &heads, &heads).map(|p| p.target_projection().clone())
}

#[allow(
    dead_code,
    reason = "the shared conformance include offers pagination assertions that not every app adapter calls"
)]
fn assert_page_round_trip(driver: &mut ui::headless::Driver, first: &str, second: &str) {
    for width in [390, 1000] {
        driver.resize((width, 750)).unwrap();
        driver.click_named("history-next-page").unwrap();
        assert!(
            driver
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(second)),
            "next page must render full second description"
        );
        driver.click_named("history-previous-page").unwrap();
        assert!(
            driver
                .a11y_nodes()
                .iter()
                .any(|(_, node)| node.label() == Some(first)),
            "previous page must render full first description: {first}"
        );
    }
}
