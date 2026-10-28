//! Pictures through the door an agent uses: placed, cropped, straightened,
//! flipped and graded as data, opened as a new artwork, the artwork cut to
//! them, and exported as PNG, JPEG, WebP and SVG.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "test assertions over exact, hand-written fixtures"
)]

use canvas::App;
use canvas::model::Shape;
use canvas::render::Renderer;
use canvas::resource::{Export, Look};
use canvas::view_ops;
use serde_json::{Value, json};
use ui::prelude::TestApp;

const RED: [u8; 4] = [210, 40, 30, 255];
const BLUE: [u8; 4] = [30, 60, 200, 255];

/// A PNG `width` by `height`, its left half red and its right half blue,
/// as a data: URI.
fn halves(width: u32, height: u32) -> String {
    use base64::Engine as _;
    let picture = image::RgbaImage::from_fn(width, height, |x, _| {
        image::Rgba(if x < width / 2 { RED } else { BLUE })
    });
    let mut png = Vec::new();
    picture
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    )
}

fn view(app: &mut TestApp<App>, action: &str, args: Value) -> Value {
    let effect = args.as_object().and_then(|args| {
        <canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(action, args).ok()
    });
    if let Some(effect) = effect {
        return app
            .effect(&effect)
            .expect("the view effect finishes")
            .value
            .unwrap_or(Value::Null);
    }

    app.call(action, args).unwrap().unwrap()
}

/// The colour a PNG look shows at the middle of `region`.
fn colour_at(app: &TestApp<App>, region: [f64; 4]) -> [u8; 4] {
    let look = Look {
        region: Some(region),
        size: Some(64),
        format: Some("png".into()),
        ..Look::default()
    };
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(app.content());
    let (picture, _) = view_ops::look(
        &mut renderer,
        &mut ui::TextRenderer::new(),
        &app.state(),
        &look,
    )
    .unwrap();
    let decoded = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
    let (w, h) = decoded.dimensions();
    decoded.get_pixel(w / 2, h / 2).0
}

fn near(a: [u8; 4], b: [u8; 4], within: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= within)
}

fn placed(app: &mut TestApp<App>) {
    app.call(
        "draw",
        json!({"marks": [{"id": "photo", "shape": {"kind": "image", "src": halves(200, 100), "x": 100, "y": 100}}]}),
    )
    .unwrap();
}

#[test]
fn a_look_job_keeps_its_snapshot_while_another_edit_is_applied() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "set-artwork",
        json!({"width": 64, "height": 64, "ground_colour": "#ff0000", "texture": "smooth"}),
    )
    .unwrap();
    let look = canvas::resource::ViewOp::Look {
        look: Look {
            size: Some(64),
            format: Some("png".into()),
            ..Default::default()
        },
    };
    let invocation = app.start_effect(&look).unwrap();
    app.call("set-artwork", json!({"ground_colour": "#0000ff"}))
        .unwrap();
    app.harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    let updates = app.effect_progress();
    let output = updates
        .iter()
        .find_map(|update| match update {
            ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{updates:?}"));
    let contract = app
        .harness()
        .resources()
        .iter()
        .find(|resource| resource.kind == "canvas.artwork")
        .unwrap()
        .actions
        .iter()
        .find(|action| action.id == "look")
        .unwrap()
        .contract
        .clone();
    assert!(matches!(
        contract.execution,
        ui::tilo_actions::ExecutionMode::Background { .. }
    ));
    contract
        .result
        .validate(output)
        .expect("the broker accepts the published terminal result");
    let hash = ui::tilo_actions::content_hash(&output.resources[0]).unwrap();
    let png = app.content().read(hash).unwrap();
    let picture = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(picture.get_pixel(32, 32).0, [255, 0, 0, 255]);
    let now = app.effect(&look).unwrap();
    assert_ne!(
        output.value.as_ref().unwrap()["revision"],
        now.value.unwrap()["revision"]
    );
    assert!(app.undo(), "only the blue-ground edit is undone");
    assert_eq!(app.state().ground.colour, "#ff0000");
}

/// In a browser page no thread renders a look: the frame loop renders its
/// picture between frames, and the look answers once it is there.
#[test]
fn a_look_rendered_between_frames_answers_with_its_picture() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "set-artwork",
        json!({"width": 64, "height": 64, "ground_colour": "#ff0000", "texture": "smooth"}),
    )
    .unwrap();
    let gpu = ui::offscreen::shared_gpu().unwrap();
    let renderer = ui::vello::Renderer::new(
        &gpu.device,
        ui::vello::RendererOptions {
            antialiasing_support: ui::vello::AaSupport::area_only(),
            ..Default::default()
        },
    )
    .unwrap();
    let mut host = ui::offscreen::pixels::PixelsHost::new();
    host.set_gpu(gpu.device, gpu.queue, renderer);
    let look = canvas::resource::ViewOp::Look {
        look: Look {
            size: Some(64),
            format: Some("png".into()),
            ..Default::default()
        },
    };
    let invocation = app.start_effect(&look).unwrap();
    assert!(host.waiting(), "the frame loop renders the look's picture");
    let answered = app
        .harness()
        .wait_until(tilo_test_env::hang_bound(), |harness| {
            host.drive();
            harness
                .job_updates(invocation)
                .iter()
                .any(ui::tilo_actions::InvocationUpdate::is_terminal)
        });
    assert!(answered, "the look never answered");
    let updates = app.effect_progress();
    let output = updates
        .iter()
        .find_map(|update| match update {
            ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => Some(output),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{updates:?}"));
    let hash = ui::tilo_actions::content_hash(&output.resources[0]).unwrap();
    let png = app.content().read(hash).unwrap();
    let picture = image::load_from_memory(&png).unwrap().to_rgba8();
    assert_eq!(picture.dimensions(), (64, 64));
    assert_eq!(picture.get_pixel(32, 32).0, [255, 0, 0, 255]);
}

#[test]
fn a_picture_is_drawn_a_unit_to_a_pixel_and_kept_by_content() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    let art = app.state();
    let mark = art.mark("photo").unwrap();
    let Shape::Image {
        src, width, height, ..
    } = &mark.shape
    else {
        panic!("{mark:?}");
    };
    assert!(ui::tilo_actions::content_hash(src).is_some(), "{src}");
    assert_eq!((*width, *height), (200.0, 100.0));
    assert!(mark.style.fill.is_none(), "a picture is not inked");
    assert!(near(colour_at(&app, [140.0, 140.0, 20.0, 20.0]), RED, 4));
    assert!(near(colour_at(&app, [240.0, 140.0, 20.0, 20.0]), BLUE, 4));
    // One side given keeps the picture's aspect.
    app.call(
        "draw",
        json!({"marks": [{"id": "small", "shape": {"kind": "image", "src": src, "x": 0, "y": 0, "width": 50}}]}),
    )
    .unwrap();
    let small = app.state();
    let Shape::Image { height, .. } = &small.mark("small").unwrap().shape else {
        panic!();
    };
    assert_eq!(*height, 25.0);
    let refused = app
        .call(
            "draw",
            json!({"marks": [{"shape": {"kind": "image", "src": "/no/such/picture.png", "x": 0, "y": 0}}]}),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("/no/such/picture.png"), "{refused}");
}

#[test]
fn a_crop_keeps_the_picture_in_place_and_shows_less_of_it() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "crop": [0.5, 0.0, 0.5, 1.0]}),
    )
    .unwrap();
    let art = app.state();
    let bounds = art.mark("photo").unwrap().bounds();
    assert_eq!((bounds.x0, bounds.width()), (200.0, 100.0));
    // Where the blue half was, it still is; the red half is gone.
    assert!(near(colour_at(&app, [240.0, 140.0, 20.0, 20.0]), BLUE, 4));
    assert!(!near(colour_at(&app, [140.0, 140.0, 20.0, 20.0]), RED, 40));
    let listed = view(&mut app, "inspect", json!({}));
    assert_eq!(
        listed["marks_bottom_first"][0]["crop"],
        json!([0.5, 0.0, 0.5, 1.0])
    );
    // [] shows it whole again, where it was.
    app.call("edit-marks", json!({"ids": ["photo"], "crop": []}))
        .unwrap();
    let bounds = app.state().mark("photo").unwrap().bounds();
    assert_eq!((bounds.x0, bounds.width()), (100.0, 200.0));
    let refused = app
        .call(
            "edit-marks",
            json!({"ids": ["photo"], "crop": [0.5, 0.0, 0.9, 1.0]}),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("crop"), "{refused}");
}

#[test]
fn flipping_mirrors_and_straightening_leaves_no_corner_bare() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("set-artwork", json!({"ground_colour": "#00ff00"}))
        .unwrap();
    placed(&mut app);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "flip": "horizontal"}),
    )
    .unwrap();
    assert!(near(colour_at(&app, [140.0, 140.0, 20.0, 20.0]), BLUE, 4));
    app.call("edit-marks", json!({"ids": ["photo"], "straighten": 12}))
        .unwrap();
    // The frame's corners show the picture, not the green ground.
    for corner in [
        [101.0, 101.0],
        [297.0, 101.0],
        [101.0, 197.0],
        [297.0, 197.0],
    ] {
        let colour = colour_at(&app, [corner[0], corner[1], 2.0, 2.0]);
        assert!(colour[1] < 120, "corner {corner:?} shows {colour:?}");
    }
    let refused = app
        .call("edit-marks", json!({"ids": ["photo"], "straighten": 60}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("-45 to 45"), "{refused}");
}

#[test]
fn adjustments_grade_the_picture_merge_and_reset() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    let before = colour_at(&app, [140.0, 140.0, 20.0, 20.0]);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "adjust": {"exposure": 1.0}}),
    )
    .unwrap();
    let brighter = colour_at(&app, [140.0, 140.0, 20.0, 20.0]);
    assert!(brighter[1] > before[1] + 10, "{before:?} -> {brighter:?}");
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "adjust": {"grayscale": true}}),
    )
    .unwrap();
    let art = app.state();
    let adjust = art.mark("photo").unwrap().style.adjust.clone().unwrap();
    assert_eq!(adjust.exposure, Some(1.0), "an edit keeps the other steps");
    let grey = colour_at(&app, [140.0, 140.0, 20.0, 20.0]);
    assert!(grey[0].abs_diff(grey[2]) <= 3, "{grey:?}");
    app.call("edit-marks", json!({"ids": ["photo"], "adjust": {}}))
        .unwrap();
    assert!(app.state().mark("photo").unwrap().style.adjust.is_none());
    assert!(near(colour_at(&app, [140.0, 140.0, 20.0, 20.0]), before, 2));
    let refused = app
        .call(
            "edit-marks",
            json!({"ids": ["photo"], "adjust": {"exposure": 9}}),
        )
        .unwrap_err()
        .to_string();
    assert!(refused.contains("exposure"), "{refused}");
}

#[test]
fn a_vignette_darkens_the_edges_not_the_middle() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    let middle = colour_at(&app, [196.0, 146.0, 8.0, 8.0]);
    let corner = colour_at(&app, [100.0, 100.0, 4.0, 4.0]);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "adjust": {"vignette": 0.8}}),
    )
    .unwrap();
    assert!(near(colour_at(&app, [196.0, 146.0, 8.0, 8.0]), middle, 6));
    let dark = colour_at(&app, [100.0, 100.0, 4.0, 4.0]);
    assert!(dark[0] + 60 < corner[0], "{corner:?} -> {dark:?}");
}

#[test]
fn a_layer_grades_everything_on_it() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "box", "shape": {"kind": "rect", "x": 10, "y": 10, "width": 80, "height": 80}, "fill": "#d22a1e"}]}),
    )
    .unwrap();
    app.call(
        "edit-layer",
        json!({"id": "layer-1", "adjust": {"invert": true}}),
    )
    .unwrap();
    let inverted = colour_at(&app, [40.0, 40.0, 20.0, 20.0]);
    assert!(near(inverted, [45, 213, 225, 255], 3), "{inverted:?}");
    let listed = view(&mut app, "inspect", json!({}));
    assert_eq!(
        listed["layers_top_first"][0]["adjust"],
        json!({"invert": true})
    );
}

#[test]
fn opening_a_picture_makes_a_new_artwork_of_it() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "old", "shape": {"kind": "rect", "x": 0, "y": 0, "width": 9, "height": 9}}]}),
    )
    .unwrap();
    app.call("open-picture", json!({"path": halves(320, 180)}))
        .unwrap();
    let art = app.state();
    assert_eq!((art.width, art.height), (320.0, 180.0));
    assert_eq!(art.layers.len(), 1);
    assert_eq!(art.marks.len(), 1);
    let picture = art.mark("picture").expect("the picture is the first mark");
    assert_eq!(picture.bounds().width(), 320.0);
    assert!(near(colour_at(&app, [20.0, 80.0, 20.0, 20.0]), RED, 4));
    app.call("undo", json!({})).unwrap();
    assert!(app.state().mark("old").is_some(), "undo brings it back");
}

#[test]
fn the_artwork_is_cut_to_a_region_or_to_what_a_mark_shows() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "crop": [0.5, 0.0, 0.5, 1.0]}),
    )
    .unwrap();
    app.call("set-artwork", json!({"fit": "photo"})).unwrap();
    let art = app.state();
    assert_eq!((art.width, art.height), (100.0, 100.0));
    assert_eq!(art.mark("photo").unwrap().bounds().origin().x, 0.0);
    app.call("set-artwork", json!({"crop": [25, 25, 50, 50]}))
        .unwrap();
    let art = app.state();
    assert_eq!((art.width, art.height), (50.0, 50.0));
    assert!(near(colour_at(&app, [10.0, 10.0, 20.0, 20.0]), BLUE, 4));
}

#[test]
fn exports_are_png_jpeg_or_webp_at_any_scale() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("open-picture", json!({"path": halves(400, 200)}))
        .unwrap();
    let art = app.state();
    let mut renderer = Renderer::default();
    renderer.pictures.read_from(app.content());
    let mut text = ui::TextRenderer::new();
    for (format, magic) in [
        ("png", &b"\x89PNG"[..]),
        ("jpeg", &b"\xff\xd8"[..]),
        ("webp", &b"RIFF"[..]),
    ] {
        let export = Export {
            scale: Some(0.5),
            format: Some(format.into()),
            quality: Some(80),
            ..Export::default()
        };
        let (picture, info) = view_ops::export(&mut renderer, &mut text, &art, &export).unwrap();
        assert!(picture.bytes.starts_with(magic), "{format}");
        assert_eq!(
            (info["width"].clone(), info["height"].clone()),
            (json!(200), json!(100))
        );
        let decoded = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
        assert!(near(decoded.get_pixel(20, 50).0, RED, 12), "{format}");
    }
    let refused = view_ops::export(
        &mut renderer,
        &mut text,
        &art,
        &Export {
            format: Some("gif".into()),
            ..Export::default()
        },
    )
    .err()
    .unwrap();
    assert!(refused.contains("png, jpeg or webp"), "{refused}");
}

/// An export written to a file an agent or script names: the format
/// follows the path, and a path that is not one is refused before anything
/// is written.
#[test]
fn exports_are_written_where_the_caller_names() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("open-picture", json!({"path": halves(40, 20)}))
        .unwrap();
    let dir = std::env::temp_dir().join(format!("canvas-export-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let jpeg = dir.join("halves.jpg");
    let answer = view(
        &mut app,
        "export",
        json!({"path": jpeg.display().to_string()}),
    );
    assert_eq!(answer["format"], "jpeg", "{answer}");
    assert_eq!(answer["path"], jpeg.display().to_string());
    assert!(std::fs::read(&jpeg).unwrap().starts_with(b"\xff\xd8"));
    let svg = dir.join("halves.svg");
    let answer = view(
        &mut app,
        "export-svg",
        json!({"path": svg.display().to_string()}),
    );
    assert!(
        answer.get("svg").is_none(),
        "written, not answered: {answer}"
    );
    assert!(std::fs::read_to_string(&svg).unwrap().starts_with("<svg"));
    for (action, path) in [
        ("export", "halves.png"),
        ("export", "/tmp/x.gif"),
        ("export-svg", "/tmp/x.png"),
    ] {
        assert!(
            app.effect(
                &<canvas::resource::ViewOp as ui::tilo_resource::Effect>::decode(
                    action,
                    json!({"path": path}).as_object().unwrap()
                )
                .unwrap()
            )
            .is_err(),
            "{action} {path} is refused"
        );
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn svg_export_embeds_the_picture_as_shown() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    app.call(
        "edit-marks",
        json!({"ids": ["photo"], "crop": [0.5, 0.0, 0.5, 1.0], "adjust": {"grayscale": true}}),
    )
    .unwrap();
    let svg = view(&mut app, "export-svg", json!({}));
    let markup = svg["svg"].as_str().unwrap();
    assert!(
        markup.contains(r#"<image x="200" y="100" width="100" height="100""#),
        "{markup}"
    );
    assert!(markup.contains("data:image/jpeg;base64,"), "{markup}");
}

#[test]
fn a_group_of_shapes_clips_a_picture() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call("set-artwork", json!({"ground_colour": "#00ff00"}))
        .unwrap();
    app.call(
        "draw",
        json!({"marks": [
            {"id": "pane-a", "group": "window", "shape": {"kind": "ellipse", "cx": 140, "cy": 150, "rx": 30, "ry": 30}, "opacity": 0},
            {"id": "pane-b", "group": "window", "shape": {"kind": "rect", "x": 230, "y": 120, "width": 50, "height": 60}, "opacity": 0},
        ]}),
    )
    .unwrap();
    placed(&mut app);
    app.call("edit-marks", json!({"ids": ["photo"], "clip": "window"}))
        .unwrap();
    assert!(near(colour_at(&app, [136.0, 146.0, 8.0, 8.0]), RED, 4));
    assert!(near(colour_at(&app, [251.0, 146.0, 8.0, 8.0]), BLUE, 4));
    let between = colour_at(&app, [196.0, 146.0, 8.0, 8.0]);
    assert!(
        between[1] > 200,
        "outside both panes the ground shows: {between:?}"
    );
    let refused = app
        .call("edit-marks", json!({"ids": ["pane-a"], "clip": "window"}))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("in group"), "{refused}");
}

#[test]
fn the_canvas_decodes_a_picture_in_the_background_and_a_look_waits_for_it() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    let art = app.state();
    let Shape::Image { src, .. } = &art.mark("photo").unwrap().shape else {
        panic!();
    };
    let mut canvas = Renderer::impatient();
    canvas.pictures.read_from(app.content());
    assert_eq!(canvas.pictures.size(src), None, "a frame does not wait");
    assert!(canvas.pictures.decoding());
    let ready = tilo_test_env::wait_for("the picture to decode", || canvas.pictures.size(src));
    assert_eq!(ready, (200, 100));
    assert!(!canvas.pictures.decoding());
    let mut look = Renderer::default();
    look.pictures.read_from(app.content());
    assert_eq!(look.pictures.size(src), Some((200, 100)), "a look waits");
}

#[test]
fn svg_export_lays_a_layers_vignette_over_its_marks() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    app.call(
        "edit-layer",
        json!({"id": "layer-1", "adjust": {"vignette": 0.5}}),
    )
    .unwrap();
    let svg = view(&mut app, "export-svg", json!({}));
    let markup = svg["svg"].as_str().unwrap();
    assert!(markup.contains(r#"<g id="lc-layer-1">"#), "{markup}");
    assert!(markup.contains(r#"mask="url(#vm-layer-1)""#), "{markup}");
    assert!(
        markup.contains(r#"<radialGradient id="vg-layer-1""#),
        "{markup}"
    );
}

#[test]
fn a_click_on_a_picture_picks_it() {
    let mut app = TestApp::<App>::new().unwrap();
    placed(&mut app);
    let art = app.state();
    let picked =
        canvas::gesture::edit::pick(&art, ui::vello::kurbo::Point::new(150.0, 150.0), false);
    assert_eq!(picked, vec!["photo".to_owned()]);
}

#[test]
fn a_picture_opened_from_the_desktop_has_an_artwork_of_its_own() {
    use base64::Engine as _;
    let data = halves(64, 32);
    let png = base64::engine::general_purpose::STANDARD
        .decode(data.split_once(";base64,").unwrap().1)
        .unwrap();
    let directory = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("canvas-launch");
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("harbour.png");
    std::fs::write(&path, &png).unwrap();
    let path = path.to_str().unwrap();
    let mut app = TestApp::<App>::launched(&[path]).unwrap();
    let art = app.state();
    assert_eq!(
        (art.title.as_str(), art.width, art.height),
        ("harbour", 64.0, 32.0)
    );
    assert!(art.mark("picture").is_some());
    let record = app.record().clone();
    app.call("set-artwork", json!({"title": "Harbour at dusk"}))
        .unwrap();
    // Opened again, the picture's artwork is as it was left.
    let app = app.relaunch(&[path]).unwrap();
    assert_eq!(app.record(), &record);
    assert_eq!(app.state().title, "Harbour at dusk");
}

#[test]
fn pixel_brush_and_eraser_keep_exact_cells_and_transparency() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "new-artwork",
        json!({"width":32,"height":32,"texture":"smooth","ground_colour":"#122a3b"}),
    )
    .unwrap();
    app.call("paint", json!({"strokes":[{"id":"pixel-line","brush":"pixel","size":1,"colour":"#dce9d9","points":[[4.2,8.7,0.1],[12.9,8.1,1.0]]}]})).unwrap();
    app.call("paint", json!({"strokes":[{"id":"pixel-clear","brush":"pixel-eraser","size":1,"points":[[8.1,8.2]]}]})).unwrap();
    let mut renderer = Renderer::default();
    let mut text = ui::TextRenderer::new();
    let (picture, _) = view_ops::export(
        &mut renderer,
        &mut text,
        &app.state(),
        &Export {
            transparent: Some(true),
            ..Export::default()
        },
    )
    .unwrap();
    let pixels = image::load_from_memory(&picture.bytes).unwrap().to_rgba8();
    for (x, y, pixel) in pixels.enumerate_pixels() {
        let filled = y == 8 && (4..=12).contains(&x) && x != 8;
        assert_eq!(
            pixel.0,
            if filled {
                [220, 233, 217, 255]
            } else {
                [0, 0, 0, 0]
            },
            "cell {x},{y}"
        );
    }
    let expected = app.state().marks;
    app.call("undo", json!({})).unwrap();
    let restored = app.state();
    assert_eq!(restored.marks.len(), 1);
    app.call("redo", json!({})).unwrap();
    assert_eq!(app.state().marks, expected);
}

#[test]
fn nearest_image_preview_preserves_every_enlarged_pixel_and_is_editable() {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "new-artwork",
        json!({"width":32,"height":32,"texture":"smooth"}),
    )
    .unwrap();
    app.call("draw", json!({"marks":[{"id":"sprite","pixelated":true,"shape":{"kind":"image","src":halves(32,32),"x":0,"y":0,"width":32,"height":32}}]})).unwrap();
    let render = |app: &TestApp<App>| {
        let mut renderer = Renderer::default();
        renderer.pictures.read_from(app.content());
        let (picture, _) = view_ops::look(
            &mut renderer,
            &mut ui::TextRenderer::new(),
            &app.state(),
            &Look {
                size: Some(1024),
                format: Some("png".into()),
                ..Look::default()
            },
        )
        .unwrap();
        image::load_from_memory(&picture.bytes).unwrap().to_rgba8()
    };
    let crisp = render(&app);
    for (x, _, pixel) in crisp.enumerate_pixels() {
        assert_eq!(pixel.0, if x < 512 { RED } else { BLUE });
    }
    app.call("edit-marks", json!({"ids":["sprite"],"pixelated":false}))
        .unwrap();
    assert!(!app.state().mark("sprite").unwrap().style.pixelated);
    let smooth = render(&app);
    assert!(
        smooth
            .pixels()
            .any(|pixel| pixel.0 != RED && pixel.0 != BLUE)
    );
    app.call("undo", json!({})).unwrap();
    assert_eq!(render(&app), crisp);
}
