//! A photo's subject and a refined edge, through the door an agent uses:
//! a selection fitted to the picture, a soft edge a mask draws as it is,
//! the old background taken out of a cut-out's edge, and what is said when
//! no subject model is set up.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test assertions over exact, hand-written fixtures and the pixels they draw"
)]

use std::cell::RefCell;
use std::rc::Rc;

use canvas::App;
use canvas::model::Shape;
use canvas::render::Renderer;
use serde_json::{Value, json};
use ui::prelude::TestApp;

fn call(app: &mut TestApp<App>, action: &str, args: Value) -> Value {
    let effect = args.as_object().and_then(|args| {
        <canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(action, args).ok()
    });
    if let Some(effect) = effect {
        return app
            .effect(&effect)
            .unwrap_or_else(|error| panic!("{action}: {error}"))
            .value
            .unwrap_or(Value::Null);
    }

    app.call(action, args)
        .unwrap_or_else(|error| panic!("{action}: {error}"))
        .unwrap_or(Value::Null)
}

/// The window shows the artwork as it is now.
fn shown(app: &TestApp<App>, renderer: &Rc<RefCell<Renderer>>) {
    renderer.borrow_mut().pictures.read_from(app.content());
    canvas::author::see(renderer, &Rc::new(app.state()));
}

/// A PNG data: URI of `width` by `height` pixels coloured by `pixel`.
fn png(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> String {
    use base64::Engine as _;
    let picture = image::RgbaImage::from_fn(width, height, |x, y| image::Rgba(pixel(x, y)));
    let mut bytes = Vec::new();
    picture
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    )
}

/// A red disc of radius 60 on white, 200 by 200, its edge anti-aliased over
/// two pixels: a product shot.
fn disc() -> String {
    png(200, 200, |x, y| {
        let distance = (f64::from(x) + 0.5 - 100.0).hypot(f64::from(y) + 0.5 - 100.0);
        let cover = (61.0 - distance).clamp(0.0, 2.0) / 2.0;
        let mix = |red: f64, white: f64| (red * cover + white * (1.0 - cover)).round() as u8;
        [mix(200.0, 255.0), mix(30.0, 255.0), mix(30.0, 255.0), 255]
    })
}

fn open_disc(app: &mut TestApp<App>) {
    call(
        app,
        "draw",
        json!({"marks": [{"id": "photo", "shape": {"kind": "image", "src": disc(), "x": 0, "y": 0, "width": 200, "height": 200}}]}),
    );
}

#[test]
fn a_rough_selection_is_refined_onto_the_pictures_edge() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    shown(&app, &renderer);
    // A circle a little too big, as a hurried lasso leaves it.
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    shown(&app, &renderer);
    let refined = call(&mut app, "find-area", json!({"refine": 8}));
    let [x, y, width, height] = [0, 1, 2, 3].map(|at| {
        refined["selection"]["bounds"][at]
            .as_f64()
            .unwrap_or_else(|| panic!("{refined}"))
    });
    // The disc runs from 39 to 161.
    assert!(
        (x - 39.0).abs() < 2.5 && (y - 39.0).abs() < 2.5,
        "{refined}"
    );
    assert!(
        (width - 122.0).abs() < 4.0 && (height - 122.0).abs() < 4.0,
        "{refined}"
    );
}

#[test]
fn refining_through_the_job_door_is_one_undo_step() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    call(&mut app, "find-area", json!({"refine": 8}));
    let after = app.state();
    assert_ne!(before.selection, after.selection);
    assert!(app.undo());
    assert_eq!(app.state(), before);
    assert!(app.redo());
    assert_eq!(app.state(), after);
}

#[test]
fn select_takes_geometry_and_points_at_find_area_for_pictures() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    let versions = app.versions();
    for args in [
        json!({"subject": true}),
        json!({"refine": 8}),
        json!({"at": [100, 100]}),
        json!({"colour": "#d03020"}),
    ] {
        assert!(app.call("select", args).is_err());
        assert_eq!(app.state(), before);
        assert_eq!(app.versions(), versions);
    }
    // The guidance names a real published operation, rather than a local
    // button callback that an agent cannot invoke.
    let resources = app.harness().resources();
    let artwork = resources
        .iter()
        .find(|resource| resource.kind == "canvas.artwork")
        .unwrap();
    let select = artwork
        .actions
        .iter()
        .find(|action| action.id == "select")
        .unwrap();
    let described = select.description.as_deref().unwrap_or_default();
    assert!(described.contains("find-area"), "{described}");
    let find = artwork
        .actions
        .iter()
        .find(|action| action.id == "find-area")
        .unwrap();
    assert_eq!(find.contract.kind, ui::tilo_actions::ActionKind::Undoable);
    assert_eq!(
        find.contract.execution,
        ui::tilo_actions::ExecutionMode::Background { cancellable: false }
    );
}

#[test]
fn a_named_edge_job_commits_one_step_as_its_original_agent() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    let versions = app.versions().len();
    let output = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("agent:canvas-finder"),
            "canvas.artwork",
            "find-area",
            json!({"refine": 8}),
        )
        .unwrap();
    assert!(output.value.unwrap()["selection"]["bounds"].is_array());
    let after = app.state();
    assert_ne!(after.selection, before.selection);
    let history = app.versions();
    assert_eq!(history.len(), versions.saturating_add(1));
    assert_eq!(
        history
            .last()
            .unwrap()
            .context
            .as_ref()
            .unwrap()
            .invoker
            .as_ref()
            .unwrap()
            .actor,
        "agent:canvas-finder"
    );
    let undo = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("agent:canvas-finder"),
            "canvas.artwork",
            "undo",
            json!({}),
        )
        .unwrap();
    assert_eq!(undo.value.unwrap()["undone"].as_array().unwrap().len(), 1);
    assert_eq!(app.state(), before);
    let redo = app
        .call_by(
            ui::tilo_actions::ActionInvoker::actor("agent:canvas-finder"),
            "canvas.artwork",
            "redo",
            json!({}),
        )
        .unwrap();
    assert_eq!(redo.value.unwrap()["redone"].as_array().unwrap().len(), 1);
    assert_eq!(app.state(), after);
}

#[test]
fn an_accepted_edge_job_allows_its_participants_published_inspect() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let effect = canvas::resource::ViewOp::FindArea {
        area: canvas::resource::FindArea {
            refine: Some(8.0),
            ..Default::default()
        },
    };
    let invocation = app.start_effect(&effect).unwrap();
    assert!(
        !app.harness()
            .job_updates(invocation)
            .iter()
            .any(ui::tilo_actions::InvocationUpdate::is_terminal)
    );
    let another_participant = app.call_by(
        ui::tilo_actions::ActionInvoker::actor("agent:canvas-reader"),
        "canvas.artwork",
        "inspect",
        json!({}),
    );
    assert!(matches!(
        another_participant,
        Err(ui::tilo_actions::DoorError::Unavailable { code, .. }) if code == "private_view"
    ));
    let output = app
        .effect(&canvas::resource::ViewOp::Inspect {
            inspect: canvas::resource::Inspect::default(),
        })
        .unwrap();
    let inspected = output.value.unwrap();
    let marks = inspected["marks_bottom_first"]
        .as_array()
        .unwrap_or_else(|| panic!("inspect did not publish its marks: {inspected}"));
    assert!(marks.iter().any(|mark| mark["id"] == "photo"));
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    assert!(
        app.harness()
            .job_updates(invocation)
            .iter()
            .any(|update| matches!(update, ui::tilo_actions::InvocationUpdate::Succeeded { .. }))
    );
    // A subsequent invocation reuses the same window without staying busy.
    app.effect(&effect).unwrap();
}

#[test]
fn a_host_cancelled_edge_job_does_not_leave_the_finder_busy() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    let versions = app.versions();
    let effect = canvas::resource::ViewOp::FindArea {
        area: canvas::resource::FindArea {
            refine: Some(8.0),
            ..Default::default()
        },
    };
    let invocation = app.start_effect(&effect).unwrap();
    // Host retirement can cancel a job even when its public contract does
    // not offer a user Cancel command. No cancellation is a success receipt.
    assert!(matches!(
        app.cancel_effect(invocation),
        Some(ui::tilo_actions::InvocationUpdate::Cancelled { .. })
    ));
    app.settle();
    assert_eq!(app.state(), before);
    assert_eq!(app.versions(), versions);
    app.effect(&effect)
        .expect("a cancelled worker must release its busy state");
    assert_ne!(app.state().selection, before.selection);
}

#[test]
fn a_running_edge_job_refuses_a_second_one_until_it_is_cancelled() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    let effect = canvas::resource::ViewOp::FindArea {
        area: canvas::resource::FindArea {
            refine: Some(8.0),
            ..Default::default()
        },
    };
    let first = app.start_effect(&effect).unwrap();
    // A finder that is genuinely running still protects itself: the refusal
    // is the in-flight guard, not a latched flag, and it edits nothing.
    let busy = app
        .start_effect(&effect)
        .expect_err("a second finder must not start over the first");
    assert!(busy.to_string().contains("already being found"), "{busy}");
    assert!(
        !app.harness()
            .job_updates(first)
            .iter()
            .any(ui::tilo_actions::InvocationUpdate::is_terminal)
    );
    assert_eq!(app.state(), before);
    // Cancelling the one that runs is what releases the guard.
    assert!(matches!(
        app.cancel_effect(first),
        Some(ui::tilo_actions::InvocationUpdate::Cancelled { .. })
    ));
    app.settle();
    app.start_effect(&effect)
        .expect("cancellation released the finder for the next operation");
}

#[test]
fn an_edge_job_refuses_to_overwrite_an_edit_made_while_it_runs() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let invocation = app
        .start_effect(&canvas::resource::ViewOp::FindArea {
            area: canvas::resource::FindArea {
                refine: Some(8.0),
                ..Default::default()
            },
        })
        .unwrap();
    call(&mut app, "select", json!({"rect": [0, 0, 10, 10]}));
    let edited = app.state();
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    let updates = app.effect_progress();
    assert!(
        updates.iter().any(|update| matches!(update,
        ui::tilo_actions::InvocationUpdate::Failed { error, .. }
        if error.message.contains("changed"))),
        "{updates:?}"
    );
    assert_eq!(app.state(), edited);
}

#[test]
fn an_edge_job_cannot_apply_after_the_person_becomes_a_viewer() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let before = app.state();
    let effect = canvas::resource::ViewOp::FindArea {
        area: canvas::resource::FindArea {
            refine: Some(8.0),
            ..Default::default()
        },
    };
    let invocation = app.start_effect(&effect).unwrap();
    app.set_role(ui::Role::Viewer);
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    assert!(
        app.effect_progress()
            .iter()
            .any(|update| matches!(update, ui::tilo_actions::InvocationUpdate::Failed { .. }))
    );
    assert_eq!(app.state(), before);
    assert!(
        app.effect(&effect).is_err(),
        "a viewer cannot start a writing job"
    );
    app.set_role(ui::Role::Owner);
    app.effect(&effect)
        .expect("permission loss did not leave the view stuck busy");
    assert_ne!(app.state().selection, before.selection);
}

#[test]
fn a_refined_edge_finished_off_thread_stays_in_the_authors_content_store() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [38, 38, 124, 124]}));
    let content = app.content();
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(content.clone());
    let scope = ui::rt::wake::WakeScope::new();
    ui::tilo_resource::content::register(&scope.handle(), content.clone());
    let prepared = {
        let _entered = scope.enter();
        canvas::subject::prepare(
            &canvas::resource::FindArea {
                refine: Some(4.0),
                ..Default::default()
            },
            &mut renderer,
            &mut ui::TextRenderer::new(),
            &app.state(),
        )
        .unwrap()
    };
    let (_, matte, _) = std::thread::spawn(move || prepared.finish().unwrap())
        .join()
        .unwrap();
    let matte = matte.expect("the refined selection keeps its soft edge");
    assert!(canvas::picture::bytes_of(&content, &matte.src).is_some());
    let other = TestApp::<App>::new().unwrap();
    assert!(canvas::picture::bytes_of(&other.content(), &matte.src).is_none());
}

#[test]
fn a_mask_of_a_refined_edge_is_soft_and_takes_the_white_out_of_the_edge() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    call(&mut app, "select", json!({"ellipse": [38, 38, 124, 124]}));
    shown(&app, &renderer);
    let refined = call(&mut app, "find-area", json!({"refine": 4}));
    assert_eq!(refined["selection"]["edge"], "refined", "{refined}");
    shown(&app, &renderer);
    call(&mut app, "use-selection", json!({"do": "mask"}));
    let art = app.state();
    let mask = art.mask_of("layer-1").expect("the layer is masked");
    let drawn: Vec<_> = art.marks_of(&mask.id).collect();
    assert!(
        matches!(drawn.as_slice(), [only] if matches!(only.shape, Shape::Image { .. })),
        "the soft edge is drawn into the mask as a picture: {drawn:?}"
    );
    // The photo's edge pixels lost the white they were mixed with.
    let photo = art.mark("photo").unwrap();
    let Shape::Image { src, .. } = &photo.shape else {
        panic!("the photo is a picture");
    };
    let bytes =
        canvas::picture::bytes_of(&app.content(), src).expect("the cleaned photo is stored");
    let other = TestApp::<App>::new().unwrap();
    assert!(
        canvas::picture::bytes_of(&other.content(), src).is_none(),
        "the cleaned photo belongs to the artwork that authored it"
    );
    let cleaned = image::load_from_memory(&bytes).unwrap().to_rgba8();
    // On the disc's edge, right of centre (x = 160), the colour is red, not pink.
    let edge = cleaned.get_pixel(160, 100).0;
    assert!(
        edge[1] < 90 && edge[2] < 90,
        "the edge pixel keeps no white: {edge:?}"
    );
    // Inside and outside are untouched.
    assert_eq!(cleaned.get_pixel(100, 100).0, [200, 30, 30, 255]);
    assert_eq!(cleaned.get_pixel(5, 5).0, [255, 255, 255, 255]);

    // Undo gives the photo back as it was.
    call(&mut app, "undo", json!({}));
    let after_undo = app.state();
    let Shape::Image { src: before, .. } = &after_undo.mark("photo").unwrap().shape else {
        panic!("the photo is a picture");
    };
    assert_ne!(before, src);
}

#[test]
fn selecting_a_subject_without_its_model_says_where_to_set_it_up() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    shown(&app, &renderer);
    let error = app
        .effect(&canvas::resource::ViewOp::FindArea {
            area: canvas::resource::FindArea {
                subject: Some(true),
                ..Default::default()
            },
        })
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("Settings") && error.contains(canvas::subject::SET_UP),
        "{error}"
    );
}

#[test]
fn refining_needs_something_selected_and_a_reach_in_range() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    shown(&app, &renderer);
    let error = app
        .effect(&canvas::resource::ViewOp::FindArea {
            area: canvas::resource::FindArea {
                refine: Some(4.0),
                ..Default::default()
            },
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("nothing is selected"), "{error}");
    call(&mut app, "select", json!({"rect": [10, 10, 50, 50]}));
    shown(&app, &renderer);
    let error = app
        .effect(&canvas::resource::ViewOp::FindArea {
            area: canvas::resource::FindArea {
                refine: Some(1000.0),
                ..Default::default()
            },
        })
        .unwrap_err()
        .to_string();
    assert!(error.contains("refine is 0 to"), "{error}");
}

#[test]
fn a_refine_right_after_an_edit_sees_that_edit_before_the_window_does() {
    let mut app = TestApp::<App>::new().unwrap();
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    // The window last showed an empty sheet.
    shown(&app, &renderer);
    open_disc(&mut app);
    call(&mut app, "select", json!({"ellipse": [34, 34, 132, 132]}));
    let refined = call(&mut app, "find-area", json!({"refine": 8}));
    let width = refined["selection"]["bounds"][2].as_f64().unwrap();
    assert!(
        (width - 122.0).abs() < 4.0,
        "the disc, not the empty sheet: {refined}"
    );
}

#[test]
fn the_wand_finds_a_pictures_colour_and_leaves_the_canvas_renderer_impatient() {
    let mut app = TestApp::<App>::new().unwrap();
    open_disc(&mut app);
    // The canvas's own renderer shows a placeholder while a picture
    // decodes; the wand waits for the picture instead.
    let mut renderer = Renderer::impatient();
    renderer.pictures.read_from(app.content());
    let query = canvas::author::ColourQuery {
        at: Some(ui::vello::kurbo::Point::new(100.0, 100.0)),
        tolerance: 0.15,
        ..canvas::author::ColourQuery::default()
    };
    let found = canvas::author::colour_region(
        &mut renderer,
        &mut ui::TextRenderer::new(),
        &app.state(),
        &query,
    )
    .unwrap();
    let area = found.area();
    assert!(
        (area - 11_690.0).abs() < 900.0,
        "the red disc, not the placeholder's frame: {area}"
    );
    assert!(
        !renderer.pictures.patient,
        "the canvas's renderer stays impatient"
    );
}

#[test]
fn a_cleaned_jpeg_photograph_stays_a_jpeg_of_about_its_size() {
    use base64::Engine as _;
    let mut app = TestApp::<App>::new().unwrap();
    let png = disc();
    let decoded = image::load_from_memory(
        &base64::engine::general_purpose::STANDARD
            .decode(png.trim_start_matches("data:image/png;base64,"))
            .unwrap(),
    )
    .unwrap()
    .to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 90)
        .encode_image(&decoded)
        .unwrap();
    let src = format!(
        "data:image/jpeg;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&jpeg)
    );
    call(
        &mut app,
        "draw",
        json!({"marks": [{"id": "photo", "shape": {"kind": "image", "src": src, "x": 0, "y": 0, "width": 200, "height": 200}}]}),
    );
    let renderer = Rc::new(RefCell::new(Renderer::default()));
    call(&mut app, "select", json!({"ellipse": [38, 38, 124, 124]}));
    shown(&app, &renderer);
    call(&mut app, "find-area", json!({"refine": 4}));
    shown(&app, &renderer);
    call(&mut app, "use-selection", json!({"do": "mask"}));
    let art = app.state();
    let Shape::Image { src, .. } = &art.mark("photo").unwrap().shape else {
        panic!("the photo is a picture");
    };
    let bytes =
        canvas::picture::bytes_of(&app.content(), src).expect("the cleaned photo is stored");
    assert_eq!(
        image::guess_format(&bytes).unwrap(),
        image::ImageFormat::Jpeg
    );
    assert!(
        bytes.len() < jpeg.len() * 2,
        "{} vs {}",
        bytes.len(),
        jpeg.len()
    );
}

#[test]
fn finder_completion_uses_a_selection_contract_the_broker_can_admit() {
    let mut app = TestApp::<App>::new().unwrap();
    let resources = app.harness().resources();
    let art = resources
        .iter()
        .find(|resource| resource.kind == "canvas.artwork")
        .unwrap();
    let select = art
        .actions
        .iter()
        .find(|action| action.id == "select")
        .unwrap();
    assert_eq!(
        select.contract.advertised_confirmation(),
        ui::tilo_actions::ConfirmationPolicy::None
    );
    let invoke = canvas::Artwork::SELECT.with(canvas::selection::Select {
        d: Some("M 4 4 L 12 4 L 12 12 L 4 12 Z".into()),
        matte: None,
        mode: None,
        feather: None,
        ..Default::default()
    });
    app.invoke(invoke).unwrap();
    assert!(
        app.state()
            .selection
            .unwrap()
            .region()
            .bounds()
            .unwrap()
            .area()
            > 0.0
    );
}
