include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
use crate::{doc::WeatherDoc, model::SavedLocation};
use ui::widget::Widget;
fn test_root() -> Box<dyn Widget> {
    Box::new(crate::WeatherApp::history_test_root())
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut d = AutoCommit::new();
    d.put(ROOT, "runtime_metadata", "initial").unwrap();
    d.get_heads();
    let mut l = SavedLocation::new(
        "Historic Oslo",
        "Oslo",
        "Norway",
        "NO",
        59.9139,
        10.7522,
        "Europe/Oslo",
    );
    WeatherDoc::upsert_location(&mut d, &l);
    WeatherDoc::select_location(&mut d, l.id);
    let selected = d.get_heads()[0];
    l.name = "Current Oslo".into();
    WeatherDoc::upsert_location(&mut d, &l);
    d.put(ROOT, "view_only", "current").unwrap();
    d.get_heads();
    (d, selected, "Historic Oslo".into())
}
fn verify_restored(d: &AutoCommit) {
    assert_eq!(WeatherDoc::read_model(d).locations[0].name, "Historic Oslo");
}
fn malformed_fixture(d: &mut AutoCommit) {
    d.put(ROOT, "saved_locations", 7).unwrap();
}
#[test]
fn production_root_and_launch_declare_saved_place_history() {
    assert_eq!(
        crate::app_root()
            .document_history_definition()
            .unwrap()
            .app_id,
        "dev.tilo.weather"
    );
    assert!(include_str!("main.rs").contains(".history(&weather::framework_history::HISTORY)"));
}

fn legacy_doc() -> AutoCommit {
    let mut d = AutoCommit::new();
    let list = d
        .put_object(ROOT, "locations", automerge::ObjType::List)
        .unwrap();
    for (i, l) in crate::sample::sample_model().locations.iter().enumerate() {
        let o = d.insert_object(&list, i, automerge::ObjType::Map).unwrap();
        for (k, v) in [
            ("id", l.id.to_string()),
            ("name", l.name.clone()),
            ("region", l.region.clone()),
            ("country", l.country.clone()),
            ("country_code", l.country_code.clone()),
            ("tz", l.timezone.clone()),
        ] {
            d.put(&o, k, v).unwrap();
        }
        d.put(&o, "lat", l.latitude).unwrap();
        d.put(&o, "lon", l.longitude).unwrap();
    }
    d.put(ROOT, "selected", 1).unwrap();
    d.get_heads();
    d
}
#[test]
fn native_legacy_and_blank_actual_live_sync_are_graph_preserving_and_effect_free() {
    let mut native = AutoCommit::new();
    WeatherDoc::write_model(&mut native, &crate::sample::sample_model());
    for mut d in vec![native, legacy_doc(), AutoCommit::new()].into_boxed_slice() {
        d.put(ROOT, "runtime_metadata", "current").unwrap();
        let heads = d.get_heads();
        let mut root = crate::app_root();
        root.sync(&mut d, &ROOT);
        assert_eq!(d.get_heads(), heads);
        let mut reopened = AutoCommit::load(&d.save()).unwrap();
        crate::app_root().sync(&mut reopened, &ROOT);
        assert_eq!(reopened.get_heads(), heads);
        assert_eq!(reopened.get_changes(&[]).len(), d.get_changes(&[]).len());
    }
}
#[test]
fn legacy_restore_preserves_exact_representation_current_selection_unknown_descendants_and_redo() {
    use tilo_actions::Action;
    let mut d = legacy_doc();
    let list = d.get(ROOT, "locations").unwrap().unwrap().1;
    let first = d.get(&list, 0).unwrap().unwrap().1;
    let extra = d
        .put_object(&first, "future_authored", automerge::ObjType::Map)
        .unwrap();
    let text = d
        .put_object(&extra, "note", automerge::ObjType::Text)
        .unwrap();
    d.splice_text(&text, 0, 0, "unknown supported text")
        .unwrap();
    let selected = d.get_heads()[0];
    crate::actions::WeatherAction::AddLocation {
        location: SavedLocation::new("New", "", "Country", "CC", 2., 3., "auto"),
    }
    .apply(&mut d)
    .unwrap();
    d.put(ROOT, "selected_location", uuid::Uuid::new_v4().to_string())
        .unwrap();
    d.put(ROOT, "selected", 77).unwrap();
    d.put(ROOT, "runtime_metadata", "current identity").unwrap();
    d.get_heads();
    let current_selection = d
        .get(ROOT, "selected_location")
        .unwrap()
        .unwrap()
        .0
        .to_str()
        .unwrap()
        .to_string();
    let mut driver = managed_driver(d, selected);
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    let state = driver.state_mut();
    assert!(state.doc.get(ROOT, "saved_locations").unwrap().is_none());
    assert_eq!(
        state
            .doc
            .get(ROOT, "selected_location")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some(current_selection.as_str())
    );
    assert_eq!(
        state.doc.get(ROOT, "selected").unwrap().unwrap().0.to_i64(),
        Some(77)
    );
    assert_eq!(
        state
            .doc
            .get(ROOT, "runtime_metadata")
            .unwrap()
            .unwrap()
            .0
            .to_str(),
        Some("current identity")
    );
    let before = state.doc.base_heads();
    test_root().sync(&mut state.doc, &ROOT);
    assert_eq!(state.doc.base_heads(), before);
    let target = state.doc.fork_at(&[selected]).unwrap();
    assert_eq!(
        project_content(&state.doc, &crate::framework_history::SCHEMA).unwrap(),
        project_content(&target, &crate::framework_history::SCHEMA).unwrap()
    );
    assert!(state.undo(None, None).is_some());
    assert!(state.doc.get(ROOT, "saved_locations").unwrap().is_some());
    assert!(state.redo(None, None).is_some());
    assert!(state.doc.get(ROOT, "saved_locations").unwrap().is_none());
    let before = state.doc.get_heads();
    let mut app = crate::WeatherApp::history_test_root();
    app.sync(&mut state.doc, &ROOT);
    assert!(app.selected_city().is_some());
    assert_eq!(state.doc.get_heads(), before);
}
#[test]
fn legacy_selection_does_not_migrate_but_next_authored_action_does() {
    use tilo_actions::Action;
    let mut d = legacy_doc();
    let id = WeatherDoc::read_model(&d).locations[0].id;
    crate::actions::WeatherAction::SelectLocation { id: id.to_string() }
        .apply(&mut d)
        .unwrap();
    assert!(d.get(ROOT, "saved_locations").unwrap().is_none());
    crate::actions::WeatherAction::RemoveLocation { id: id.to_string() }
        .apply(&mut d)
        .unwrap();
    assert!(d.get(ROOT, "saved_locations").unwrap().is_some());
    assert!(
        !WeatherDoc::read_model(&d)
            .locations
            .iter()
            .any(|l| l.id == id)
    );
}
#[test]
fn invalid_coordinates_identity_duplicates_types_and_conflicts_refuse_before_projection() {
    for defect in 0..8 {
        let (mut d, _, _) = fixture();
        let places = d.get(ROOT, "saved_locations").unwrap().unwrap().1;
        let key = d.keys(&places).next().unwrap();
        let row = d.get(&places, &key).unwrap().unwrap().1;
        match defect {
            0 => {
                d.put(&row, "lat", f64::NAN).unwrap();
            }
            1 => {
                d.put(&row, "lon", 181.).unwrap();
            }
            2 => {
                d.put(&row, "id", uuid::Uuid::new_v4().to_string()).unwrap();
            }
            3 => {
                let order = d.get(ROOT, "location_order").unwrap().unwrap().1;
                d.insert(&order, 1, key).unwrap();
            }
            4 => {
                d.put(&row, "name", false).unwrap();
            }
            5 => {
                d.put_object(&row, "future_counter", automerge::ObjType::Map)
                    .unwrap();
                let o = d.get(&row, "future_counter").unwrap().unwrap().1;
                d.put(&o, "counter", automerge::ScalarValue::counter(1))
                    .unwrap();
            }
            6 => {
                let mut fork = d.fork();
                d.put(&row, "name", "one").unwrap();
                fork.put(&row, "name", "two").unwrap();
                d.merge(&mut fork).unwrap();
            }
            _ => {
                d.put(ROOT, "locations", 7).unwrap();
            }
        }
        let heads = d.get_heads();
        assert!(
            crate::framework_history::validate(&d).is_err(),
            "defect {defect}"
        );
        assert!(
            prepare_content_restore(&d, &crate::framework_history::SCHEMA, &heads, &heads).is_err()
        );
        assert_eq!(d.get_heads(), heads);
    }
}
#[test]
fn restoring_blank_removes_authored_places_without_reseeding() {
    let mut d = AutoCommit::new();
    d.put(ROOT, "runtime_metadata", "identity").unwrap();
    let selected = d.get_heads()[0];
    WeatherDoc::write_model(&mut d, &crate::sample::sample_model());
    d.get_heads();
    let mut driver = managed_driver(d, selected);
    driver.click_named("history-restore").unwrap();
    driver.key("Enter");
    driver.click_named("history-back-live").unwrap();
    assert!(
        WeatherDoc::read_model(&driver.state_mut().doc)
            .locations
            .is_empty()
    );
    assert!(
        driver
            .state_mut()
            .doc
            .get(ROOT, "saved_locations")
            .unwrap()
            .is_none()
    );
    let heads = driver.state_mut().doc.get_heads();
    crate::app_root().sync(&mut driver.state_mut().doc, &ROOT);
    assert_eq!(driver.state_mut().doc.get_heads(), heads);
}

#[test]
fn exact_weather_review_refuses_another_record_owner() {
    let (mut d, selected, _) = fixture();
    let heads = d.get_heads();
    let resource = HistoryResource::new(
        "profile",
        "workspace",
        "dev.tilo.weather",
        ui::RecordId::for_tab(55),
    );
    let mut controller = HistoryController::new(resource, &d).unwrap();
    let entry = ui::history_source::entry_id_from_hash(&selected.to_string());
    controller.select(&d, &[entry]).unwrap();
    let review = controller.review_restore().unwrap();
    let mut other = HistoryController::new(
        HistoryResource::new(
            "profile",
            "workspace",
            "dev.tilo.weather",
            ui::RecordId::for_tab(56),
        ),
        &d,
    )
    .unwrap();
    other.select(&d, &[entry]).unwrap();
    assert_eq!(
        other.validate_restore(&review, &d),
        Err(HistoryError::ResourceMismatch)
    );
    assert_eq!(d.get_heads(), heads);
}
#[test]
fn deliberate_legacy_migration_uses_one_canonical_action_and_undo_boundary() {
    let mut state = ui::State::from_doc(legacy_doc());
    let old = project_content(&state.doc, &crate::framework_history::SCHEMA).unwrap();
    let before = state.doc.get_heads();
    let mut actions = ui::ActionHost::new();
    actions
        .try_dispatch(
            &mut state.doc,
            crate::actions::WEATHER_ACTION_TYPE,
            &crate::actions::WeatherAction::AddLocation {
                location: SavedLocation::new("Added", "", "", "", 1., 2., "auto"),
            },
        )
        .unwrap();
    assert_eq!(state.doc.get_changes(&before).len(), 1);
    state.record_committed_action(actions.take_committed().unwrap(), None, None);
    assert!(state.undo(None, None).is_some());
    assert_eq!(
        project_content(&state.doc, &crate::framework_history::SCHEMA).unwrap(),
        old
    );
    assert!(state.undo(None, None).is_none());
    assert!(state.redo(None, None).is_some());
    assert!(WeatherDoc::has_keyed_schema(&state.doc));
}
#[test]
fn pinned_preview_recreation_retains_local_navigation_and_never_reads_live_updates() {
    let mut d = legacy_doc();
    let selected = d.get_heads()[0];
    let resource = HistoryResource::new(
        "profile",
        "workspace",
        "dev.tilo.weather",
        ui::RecordId::for_tab(1),
    );
    let mut c = HistoryController::new(resource, &d).unwrap();
    c.select(
        &d,
        &[ui::history_source::entry_id_from_hash(
            &selected.to_string(),
        )],
    )
    .unwrap();
    let snapshot = c.snapshot().unwrap().clone();
    let create = definition().scopes[0]
        .presentation
        .preview_factory()
        .unwrap();
    let mut p = create(PreviewInput {
        snapshot: snapshot.clone(),
        initial_view: PreviewView::default(),
        assets: FrozenPreviewAssets::default(),
    })
    .unwrap();
    let last = p.accessibility().pages.last().unwrap().0.clone();
    p.navigate(PreviewNavigation::SelectPage(last));
    p.navigate(PreviewNavigation::Scroll { dx: 0., dy: -120. });
    let mut tr = ui::TextRenderer::new();
    let theme = ui::Theme::dark();
    let bounds = vello::kurbo::Rect::new(0., 0., 320., 240.);
    p.layout(bounds, &mut tr, &theme).unwrap();
    let saved = p.view();
    let description = p.accessibility().description;
    let mut recreated = create(PreviewInput {
        snapshot,
        initial_view: saved.clone(),
        assets: FrozenPreviewAssets::default(),
    })
    .unwrap();
    recreated.layout(bounds, &mut tr, &theme).unwrap();
    assert_eq!(recreated.view(), saved);
    assert_eq!(recreated.accessibility().description, description);
    WeatherDoc::ensure_schema(&mut d);
    d.get_heads();
    c.refresh(&d).unwrap();
    assert!(c.has_live_updates());
    assert_eq!(p.accessibility().description, description);
    let before = d.get_heads();
    let mut scene = vello::Scene::new();
    recreated.draw(&mut scene, vello::kurbo::Affine::IDENTITY, &mut tr, &theme);
    assert_eq!(d.get_heads(), before);
}
#[test]
fn normal_saver_reopens_restored_native_legacy_and_blank_through_actual_live_sync() {
    if let Ok(stage) = std::env::var("QOS_WEATHER_SAVED_STAGE") {
        let kind = std::env::var("QOS_WEATHER_SAVED_KIND").unwrap();
        let (mut state, mut saver) = ui::persist::try_open("dev.tilo.weather").unwrap();
        if stage == "write" {
            let mut target = match kind.as_str() {
                "legacy" => legacy_doc(),
                "native" => {
                    let mut d = AutoCommit::new();
                    WeatherDoc::write_model(&mut d, &crate::sample::sample_model());
                    d
                }
                _ => AutoCommit::new(),
            };
            target
                .put(ROOT, "runtime_metadata", "current identity")
                .unwrap();
            state.doc.merge(&mut target).unwrap();
            let selected = state.doc.get_heads()[0];
            WeatherDoc::upsert_location(
                &mut state.doc,
                &SavedLocation::new("Later", "", "", "", 1., 2., "auto"),
            );
            state.doc.get_heads();
            saver.request(&mut state.doc);
            saver.try_flush().unwrap();
            let mut driver = managed_driver(state.doc, selected);
            driver.click_named("history-restore").unwrap();
            driver.key("Enter");
            saver.request(&mut driver.state_mut().doc);
            saver.try_flush().unwrap();
            std::fs::write(
                saver.document_path().with_extension("selected"),
                selected.to_string(),
            )
            .unwrap();
        } else {
            let heads = state.doc.get_heads();
            crate::app_root().sync(&mut state.doc, &ROOT);
            assert_eq!(state.doc.get_heads(), heads);
            let selected =
                std::fs::read_to_string(saver.document_path().with_extension("selected"))
                    .unwrap()
                    .parse()
                    .unwrap();
            let target = state.doc.fork_at(&[selected]).unwrap();
            assert_eq!(
                project_content(&state.doc, &crate::framework_history::SCHEMA).unwrap(),
                project_content(&target, &crate::framework_history::SCHEMA).unwrap()
            );
            assert_eq!(
                state
                    .doc
                    .get(ROOT, "runtime_metadata")
                    .unwrap()
                    .unwrap()
                    .0
                    .to_str(),
                Some("current identity")
            );
        }
        return;
    }
    for kind in ["native", "legacy", "blank"] {
        let dir =
            std::env::temp_dir().join(format!("weather-history-{kind}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        for stage in ["write", "read"] {
            let out=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","framework_history_tests::normal_saver_reopens_restored_native_legacy_and_blank_through_actual_live_sync","--nocapture"]).env("QOS_WEATHER_SAVED_STAGE",stage).env("QOS_WEATHER_SAVED_KIND",kind).env("XDG_DATA_HOME",&dir).env("XDG_CONFIG_HOME",&dir).env("TILO_PACKAGE_ID","dev.tilo.weather").output().unwrap();
            assert!(
                out.status.success(),
                "{kind}/{stage}: {} {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn legacy_next_content_action_preserves_the_current_stable_selection() {
    use tilo_actions::Action;
    let mut d = legacy_doc();
    let chosen = WeatherDoc::read_model(&d).locations[0].id;
    crate::actions::WeatherAction::SelectLocation {
        id: chosen.to_string(),
    }
    .apply(&mut d)
    .unwrap();
    crate::actions::WeatherAction::AddLocation {
        location: SavedLocation::new("New", "", "", "", 1., 2., "auto"),
    }
    .apply(&mut d)
    .unwrap();
    assert_eq!(
        WeatherDoc::read_model(&d).selected_location().unwrap().id,
        chosen
    );
}
