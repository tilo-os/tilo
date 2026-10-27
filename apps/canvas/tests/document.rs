#![allow(
    clippy::unwrap_used,
    reason = "test assertions over exact, hand-written fixtures"
)]

mod support;

use canvas::App;
use ui::prelude::TestApp;

/// A view of the artwork, waited for as the job it is.
fn view(app: &mut TestApp<App>, id: &str, args: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(args) = args else {
        return serde_json::Value::Null;
    };
    let op = <canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(id, &args).unwrap();
    app.effect(&op)
        .unwrap()
        .value
        .unwrap_or(serde_json::Value::Null)
}

fn labels(app: &TestApp<App>) -> Vec<String> {
    app.versions()
        .iter()
        .map(|version| format!("{version:?}"))
        .collect()
}

#[test]
fn naming_the_artwork_is_one_step_of_history() {
    let mut app = TestApp::<App>::new().unwrap();
    let before = app.versions().len();
    // The window bar's location is the title: committing it renames the
    // artwork through its own action.
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Commit {
            text: "Dawn".to_owned(),
        });
    assert_eq!(app.state().title, "Dawn");
    let after = labels(&app);
    assert_eq!(
        after.len() - before,
        1,
        "typing a title and pressing Enter is one step: {after:#?}"
    );
}

#[test]
fn a_new_artwork_starts_from_the_dialog() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-new").unwrap();
    app.harness().click_named("canvas-new-size-a4").unwrap();
    app.harness()
        .click_named("canvas-new-start-watercolour")
        .unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().type_text("Harbour");
    app.harness().key("Enter");
    let art = app.state();
    assert_eq!(
        (art.title.as_str(), art.width, art.height),
        ("Harbour", 3508.0, 2480.0)
    );
    assert_eq!(art.ground.texture, "cold-press");
    assert_eq!(art.layers.len(), 1);
    assert_eq!(art.layers[0].medium, "watercolour");
    assert_eq!(art.layers[0].name, "Watercolour");
}

/// What an artwork is, without the orders its marks and layers were given
/// where they were written.
fn content(art: &canvas::Artwork) -> serde_json::Value {
    let mut file = serde_json::to_value(canvas::document::ArtworkFile::of(art)).unwrap();
    file.as_object_mut().unwrap().remove("format");
    file
}

#[test]
fn an_agent_saves_starts_over_and_opens_the_artwork_again() {
    let mut app = TestApp::<App>::new().unwrap();
    for action in canvas::sample::landscape() {
        app.invoke(action).unwrap();
    }
    app.call("set-artwork", serde_json::json!({"title": "Valley"}))
        .unwrap();
    let painted = app.state();
    let saved = app
        .call("save-artwork", serde_json::json!({}))
        .unwrap()
        .unwrap();
    let address = saved["content"].as_str().unwrap().to_owned();

    // A path in Files, too.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    app.call(
        "save-artwork",
        serde_json::json!({"path": path.display().to_string()}),
    )
    .unwrap();
    assert!(path.exists());

    app.call(
        "new-artwork",
        serde_json::json!({"size": "square", "start": "oil", "title": "Fresh"}),
    )
    .unwrap();
    let fresh = app.state();
    assert_eq!(
        (
            fresh.title.as_str(),
            fresh.width,
            fresh.height,
            fresh.marks.len()
        ),
        ("Fresh", 2048.0, 2048.0, 0)
    );
    assert_eq!(fresh.ground.texture, "linen");
    assert_eq!(fresh.layers[0].medium, "oil");

    // One undo brings the painting back.
    assert!(app.undo());
    assert_eq!(content(&app.state()), content(&painted));

    app.call("new-artwork", serde_json::json!({})).unwrap();
    app.call("open-artwork", serde_json::json!({"file": address}))
        .unwrap();
    assert_eq!(content(&app.state()), content(&painted));

    app.call("new-artwork", serde_json::json!({})).unwrap();
    app.call(
        "open-artwork",
        serde_json::json!({"file": path.display().to_string()}),
    )
    .unwrap();
    assert_eq!(content(&app.state()), content(&painted));
}

#[test]
fn a_wrong_file_or_size_says_what_is_wrong() {
    let mut app = TestApp::<App>::new().unwrap();
    let error = app
        .call("new-artwork", serde_json::json!({"size": "a3"}))
        .unwrap_err()
        .to_string();
    assert!(error.contains("screen, a4, letter"), "{error}");
    let error = app
        .call(
            "open-artwork",
            serde_json::json!({"file": "{\"format\": \"other\"}"}),
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("not a Canvas artwork"), "{error}");
}

#[test]
fn a_tools_key_works_after_a_click_in_the_rail_and_a_tool_click_after_a_drag_lands() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-close").unwrap();
    let canvas = app.harness().targets()["canvas"];
    let rail = app.harness().targets()["Tool"];
    let (x, y) = (canvas.center().x, canvas.center().y);
    // Click the select tool in the rail (its top segment), so the rail has
    // the focus; then R picks up the rectangle anyway.
    app.harness().click_at(rail.center().x, rail.y0 + 20.0);
    app.harness().key("r");
    app.harness()
        .drag_at((x - 60.0, y - 40.0), (x + 60.0, y + 40.0));
    let art = app.state();
    assert_eq!(
        art.marks
            .iter()
            .filter(|mark| mark.shape.kind() == "rect")
            .count(),
        1,
        "R after a click in the rail drew a rectangle"
    );
    // Right after a drag on the canvas, a click on the ellipse in the rail
    // is taken, and the next drag draws an ellipse.
    app.harness()
        .drag_at((x - 100.0, y - 100.0), (x - 80.0, y - 80.0));
    let snapshot = app.harness().inspection();
    let label = ui::i18n::I18nManager::lookup_plain("canvas-tool-ellipse");
    let target = support::named_bounds(&snapshot.root, &label).expect("ellipse control is visible");
    app.harness().click_at(target.center().x, target.center().y);
    app.harness()
        .drag_at((x + 80.0, y + 60.0), (x + 160.0, y + 120.0));
    assert!(
        app.state()
            .marks
            .iter()
            .any(|mark| mark.shape.kind() == "ellipse"),
        "the ellipse tool clicked right after a drag drew an ellipse"
    );
}

#[test]
fn a_person_opens_a_recent_artwork_from_the_welcome() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    {
        let mut painter = TestApp::<App>::new().unwrap();
        for action in canvas::sample::landscape() {
            painter.invoke(action).unwrap();
        }
        painter
            .call(
                "save-artwork",
                serde_json::json!({"path": path.display().to_string()}),
            )
            .unwrap();
    }
    let painted = canvas::document::read(&path).unwrap();
    let mut app = TestApp::<App>::new().unwrap();
    app.settings::<canvas::document::RecentArtworks>()
        .update(|recents| {
            recents.remember(canvas::document::Recent {
                path: path.display().to_string(),
                title: "Valley".into(),
                width: painted.width,
                height: painted.height,
                at_ms: 1,
                thumbnail: None,
            });
        })
        .unwrap();
    let before = app.versions().len();
    app.harness().key("Shift");
    app.harness().click_named("canvas-recent-1").unwrap();
    app.harness().key("Shift");
    assert_eq!(
        app.state().marks.len(),
        painted.marks.len(),
        "the recent artwork opened"
    );
    assert_eq!(app.versions().len(), before + 1, "opening is one step");
}

/// Every Canvas window starts an artwork of its own, so one made in a
/// window that was closed is reached again from the next window's welcome,
/// whether or not it was ever saved to a file.
#[test]
fn an_artwork_drawn_in_a_closed_window_opens_from_the_next_welcome() {
    use canvas::actions::{Draw, NewMark};
    use canvas::model::Shape;

    let mut first = TestApp::<App>::new().unwrap();
    first
        .invoke(canvas::Artwork::DRAW.with(Draw {
            layer: None,
            marks: vec![NewMark {
                id: Some("box".into()),
                ..NewMark::new(Shape::Rect {
                    x: 100.0,
                    y: 100.0,
                    width: 300.0,
                    height: 200.0,
                    radius: 0.0,
                })
            }],
        }))
        .unwrap();
    let drawn = first.record().clone();

    let mut app = first.another_window().unwrap();
    assert_ne!(
        app.record(),
        &drawn,
        "a new window starts an artwork of its own"
    );
    assert!(app.state().marks.is_empty(), "and it starts blank");
    app.harness().key("Shift");
    let earlier = "canvas-recent-artwork-0";
    assert!(
        app.harness().targets().contains_key(earlier),
        "the welcome lists the earlier artwork among the recent ones"
    );
    app.harness().click_named(earlier).unwrap();
    app.harness().key("Shift");
    assert_eq!(
        app.record(),
        &drawn,
        "the window opened the earlier artwork"
    );
    assert!(
        app.state().marks.iter().any(|mark| mark.id == "box"),
        "and shows its drawing"
    );
}

#[test]
fn the_panels_stay_put_away_on_a_narrow_window() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().resize((868, 772)).unwrap();
    app.harness().key("Shift");
    let shown = |app: &mut TestApp<App>| app.harness().targets().contains_key("canvas-dock");
    let before = shown(&mut app);
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: "canvas.tools".to_owned(),
        });
    for _ in 0..4 {
        app.harness().key("Shift");
    }
    assert_ne!(
        shown(&mut app),
        before,
        "the toggle shows or hides the panels"
    );
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: "canvas.tools".to_owned(),
        });
    for _ in 0..4 {
        app.harness().key("Shift");
    }
    assert_eq!(shown(&mut app), before, "and brings them back");
}

#[test]
fn an_artwork_file_opens_from_files_as_its_own_record() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    {
        let mut painter = TestApp::<App>::new().unwrap();
        for action in canvas::sample::landscape() {
            painter.invoke(action).unwrap();
        }
        painter
            .call(
                "save-artwork",
                serde_json::json!({"path": path.display().to_string()}),
            )
            .unwrap();
    }
    let painted = canvas::document::read(&path).unwrap();
    let opened = TestApp::<App>::launched(&[&path.display().to_string()]).unwrap();
    assert_eq!(opened.state().marks.len(), painted.marks.len());
    assert_eq!(
        opened
            .versions()
            .iter()
            .filter(|version| version.context.is_some())
            .count(),
        1,
        "opening the file is one step"
    );

    // Handed over by Files' "open with", into the window's artwork.
    let (file, _receipt) = ui::application::OpenFile::new(
        "Valley.artwork",
        canvas::document::MEDIA_TYPE,
        std::fs::read(&path).unwrap(),
    )
    .unwrap();
    let handed = TestApp::<App>::handed(file).unwrap();
    assert_eq!(handed.state().marks.len(), painted.marks.len());
}

#[test]
fn folders_keep_paint_order_and_round_trip_through_history_and_saved_artwork() {
    use serde_json::json;
    let mut app = TestApp::<App>::new().unwrap();
    app.call("add-layer", json!({"id":"hut", "name":"Hut"}))
        .unwrap();
    let before = app.state();
    app.call("checkpoint", json!({"name":"before folder"}))
        .unwrap();
    app.call("edit-layer", json!({"id":"hut", "group":"Night scene"}))
        .unwrap();
    assert_eq!(app.state().layer("hut").unwrap().group, "Night scene");
    assert_eq!(
        app.state()
            .layers
            .iter()
            .map(|l| &l.order)
            .collect::<Vec<_>>(),
        before.layers.iter().map(|l| &l.order).collect::<Vec<_>>()
    );
    let diff = view(
        &mut app,
        "compare-versions",
        json!({"from":"before folder"}),
    );
    assert_eq!(diff["layers"]["changed"]["hut"], json!(["group"]));
    assert!(app.undo());
    assert_eq!(app.state().layer("hut").unwrap().group, "");
    assert!(app.redo());
    let saved = app.call("save-artwork", json!({})).unwrap().unwrap();
    app.call("new-artwork", json!({})).unwrap();
    app.call("open-artwork", json!({"file":saved["content"]}))
        .unwrap();
    assert_eq!(app.state().layer("hut").unwrap().group, "Night scene");
    app.call("edit-layer", json!({"id":"hut","group":""}))
        .unwrap();
    assert!(app.state().layer("hut").unwrap().group.is_empty());
}

#[test]
fn the_pixel_template_starts_small_transparent_and_uses_a_cell_brush() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-new").unwrap();
    app.harness()
        .click_named("canvas-new-size-pixel-32")
        .unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().type_text("Sprite");
    app.harness().key("Enter");
    let art = app.state();
    assert_eq!((art.width, art.height), (32.0, 32.0));
    assert_eq!(art.ground.texture, "smooth");
    assert_eq!(art.ground.colour, "#ffffff00");
    let canvas = app.harness().targets()["canvas"];
    app.harness().click_at(canvas.center().x, canvas.center().y);
    let art = app.state();
    assert_eq!(art.marks.len(), 1);
    assert_eq!(art.marks[0].style.brush.as_deref(), Some("pixel"));
}

#[test]
fn naming_a_layer_folder_is_one_edit_with_one_undo_step() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("add-layer", serde_json::json!({"id":"hut", "name":"Hut"}))
        .unwrap();
    app.harness().resize((1200, 900)).unwrap();
    app.harness().click_named("canvas-welcome-close").unwrap();
    let before = app.state();
    let mut revealed = false;
    for _ in 0..20 {
        let target = app.harness().targets()["canvas-layer-folder"];
        if target.y1 <= 900.0 {
            app.harness().click_named("canvas-layer-folder").unwrap();
            revealed = true;
            break;
        }
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(target.center().x, 850.0));
        app.harness().scroll(0.0, -2.0);
    }
    assert!(
        revealed,
        "the folder field is reachable through the layer panel"
    );
    app.harness().type_text("Night scene");
    assert_eq!(
        app.state(),
        before,
        "letters remain local until the rename is committed"
    );
    app.harness().key("Enter");
    assert_eq!(app.state().layer("hut").unwrap().group, "Night scene");
    assert!(app.undo());
    assert_eq!(
        app.state(),
        before,
        "one undo removes the whole folder assignment"
    );
    assert!(app.redo());
    assert_eq!(app.state().layer("hut").unwrap().group, "Night scene");
}

#[test]
fn a_pixel_template_allows_an_explicit_paper_ground() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-new").unwrap();
    app.harness()
        .click_named("canvas-new-size-pixel-32")
        .unwrap();
    app.harness().click_named("canvas-new-start-paper").unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().type_text("Paper sprite");
    app.harness().key("Enter");
    assert_eq!(app.state().width.to_bits(), 32.0_f64.to_bits());
    assert_eq!(app.state().ground.texture, "paper");
    assert_eq!(app.state().ground.colour, "#f4efe3");
}

#[test]
fn a_small_vector_icon_keeps_vector_tools_in_the_automatic_workspace() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("new-artwork", serde_json::json!({"width":32,"height":32}))
        .unwrap();
    app.call("draw", serde_json::json!({"marks":[{"id":"icon","shape":{"kind":"ellipse","cx":16,"cy":16,"rx":10,"ry":10},"fill":"#123456"}]})).unwrap();
    let snapshot = app.harness().inspection();
    let name = ui::i18n::I18nManager::lookup_plain(canvas::tools::Tool::Text.label());
    assert!(
        support::named_bounds(&snapshot.root, &name).is_some(),
        "small vector icons still offer lettering"
    );
}

#[test]
fn all_project_choices_fit_and_accept_pointer_input_at_the_live_window_size() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().resize((868, 772)).unwrap();
    for (key, pixel) in [
        ("canvas-workspace-pixel", true),
        ("canvas-workspace-photo", false),
        ("canvas-workspace-vector", false),
        ("canvas-workspace-auto", false),
    ] {
        let snapshot = app.harness().inspection();
        let label = ui::i18n::I18nManager::lookup_plain(key);
        let bounds = support::named_bounds(&snapshot.root, &label).unwrap();
        assert!(
            bounds.x0 >= 0.0 && bounds.x1 <= 868.0 && bounds.y1 <= 772.0,
            "{label} is fully visible: {bounds:?}"
        );
        assert!(bounds.width() >= 44.0 && bounds.height() >= 28.0);
        app.harness().click_at(bounds.center().x, bounds.center().y);
        if pixel {
            let canvas = app.harness().targets()["canvas"];
            app.harness().click_at(canvas.center().x, canvas.center().y);
            assert_eq!(app.state().marks[0].style.brush.as_deref(), Some("pixel"));
        } else {
            let snapshot = app.harness().inspection();
            let name = ui::i18n::I18nManager::lookup_plain(canvas::tools::Tool::Select.label());
            assert!(support::named_bounds(&snapshot.root, &name).is_some());
        }
    }
}

#[test]
fn starting_a_regular_sheet_after_a_sprite_resets_the_pixel_brush() {
    let mut app = TestApp::<App>::new().unwrap();
    app.harness().click_named("canvas-welcome-new").unwrap();
    app.harness()
        .click_named("canvas-new-size-pixel-32")
        .unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().key("Enter");
    // New artwork, from the artwork menu the window bar holds.
    app.harness()
        .press_window_slot(ui::window_toolbar::ToolbarEvent::Action {
            id: "canvas.carry-out.new".to_owned(),
        });
    app.harness().click_named("canvas-new-size-a4").unwrap();
    app.harness().click_named("canvas-new-start-paper").unwrap();
    app.harness().click_named("canvas-new-title").unwrap();
    app.harness().key("Enter");
    app.harness().key("b");
    let canvas = app.harness().targets()["canvas"];
    app.harness().click_at(canvas.center().x, canvas.center().y);
    assert_eq!(app.state().marks.len(), 1);
    assert_ne!(app.state().marks[0].style.brush.as_deref(), Some("pixel"));
}

/// Open and Save as go through the host by contents: the chosen file's
/// bytes come back (a browser page has no path to hand over) and the save
/// goes to a destination, so both work on every host.
#[test]
fn open_and_save_as_read_and_write_through_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("Valley.artwork");
    {
        let mut painter = TestApp::<App>::new().unwrap();
        for action in canvas::sample::landscape() {
            painter.invoke(action).unwrap();
        }
        painter
            .call(
                "save-artwork",
                serde_json::json!({"path": path.display().to_string()}),
            )
            .unwrap();
    }
    let painted = canvas::document::read(&path).unwrap();

    let mut app = TestApp::<App>::new().unwrap();
    let (services, recorder) = ui::host::HostServices::recording();
    let copy = dir.path().join("Valley copy.artwork");
    ui::host::scoped(&services, || {
        let work = ui::background::Pumped::new();
        let _work = work.background().enter();
        app.harness()
            .perform_named("canvas", &ui::a11y::Action::Focus)
            .unwrap();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(path.clone()));
        app.harness().set_modifiers(&["Ctrl".to_owned()]);
        app.harness().key("o");
        app.harness().set_modifiers(&[]);
        app.settle();
        work.run_pending();
        app.settle();
        assert_eq!(
            app.state().marks.len(),
            painted.marks.len(),
            "the chosen artwork opened from its contents"
        );

        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(copy.clone()));
        app.harness()
            .set_modifiers(&["Ctrl".to_owned(), "Shift".to_owned()]);
        app.harness().key("S");
        app.harness().set_modifiers(&[]);
        app.settle();
        work.run_pending();
        app.settle();
        work.run_pending();
        app.settle();
    });
    let saved = canvas::document::read(&copy).unwrap();
    assert_eq!(saved.marks.len(), painted.marks.len(), "Save as wrote it");
    assert_eq!(
        recorder
            .pick_requests()
            .iter()
            .map(ui::portal::PickRequest::is_open)
            .collect::<Vec<_>>(),
        [true, false],
        "an Open chooser, then a Save chooser"
    );
}

/// A host whose files have no paths (a browser page): every Save chooser
/// answers with a place it keeps, and each write to it is recorded.
#[derive(Default)]
struct PathlessHost {
    asked: std::sync::Mutex<usize>,
    written: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl ui::host::FilePicker for PathlessHost {
    fn pick(&self, _: ui::portal::PickRequest) -> ui::portal::PortalChannel {
        answered(ui::portal::PortalOutcome::Failed("no paths".into()))
    }
    fn pick_multiple(&self, _: ui::portal::PickRequest) -> ui::portal::MultiChannel {
        answered(ui::portal::MultiOutcome::Failed("no paths".into()))
    }
    fn open_file(&self, _: ui::portal::PickRequest) -> ui::portal::OpenFileChannel {
        answered(ui::portal::OpenFileOutcome::Failed("no paths".into()))
    }
    fn launch(&self, _: ui::portal::LaunchRequest) -> ui::portal::LaunchChannel {
        answered(ui::portal::LaunchOutcome::Failed("no paths".into()))
    }
    fn save_destination(&self, request: ui::portal::PickRequest) -> ui::portal::DestinationChannel {
        let mut asked = self.asked.lock().unwrap();
        *asked = asked.saturating_add(1);
        drop(asked);
        let written = std::sync::Arc::clone(&self.written);
        let name = request.suggested_name().unwrap_or_default().to_owned();
        ui::portal::DestinationChannel::answered(ui::portal::DestinationOutcome::Chosen(
            ui::portal::SaveDestination::kept(name, move |bytes| {
                written.lock().unwrap().push(bytes);
                answered(ui::portal::SaveOutcome::Saved)
            }),
        ))
    }
}

fn answered<T>(outcome: T) -> std::sync::mpsc::Receiver<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}

/// Where the chosen place has no path (a browser page's Save dialog file),
/// the first Save asks where, every later Save writes there quietly, and
/// only Save as asks again.
#[test]
fn a_place_with_no_path_is_kept_and_later_saves_write_there_quietly() {
    let host = std::sync::Arc::new(PathlessHost::default());
    let services = ui::host::HostServices::null().with_files(host.clone());
    let mut app = TestApp::<App>::new().unwrap();
    let press = |app: &mut TestApp<App>, modifiers: &[&str], key: &str| {
        let modifiers: Vec<String> = modifiers.iter().map(|m| (*m).to_owned()).collect();
        app.harness().set_modifiers(&modifiers);
        app.harness().key(key);
        app.harness().set_modifiers(&[]);
        app.settle();
        app.settle();
    };
    ui::host::scoped(&services, || {
        app.harness()
            .perform_named("canvas", &ui::a11y::Action::Focus)
            .unwrap();
        press(&mut app, &["Ctrl"], "s");
        assert_eq!(*host.asked.lock().unwrap(), 1, "the first Save asks where");
        assert_eq!(host.written.lock().unwrap().len(), 1, "and writes there");

        press(&mut app, &["Ctrl"], "s");
        assert_eq!(*host.asked.lock().unwrap(), 1, "the next Save asks nothing");
        assert_eq!(
            host.written.lock().unwrap().len(),
            2,
            "and writes there again"
        );

        press(&mut app, &["Ctrl", "Shift"], "S");
        assert_eq!(*host.asked.lock().unwrap(), 2, "Save as asks again");
    });
}
