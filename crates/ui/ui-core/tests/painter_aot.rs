//! A theme module changed while a session runs: from the new module to the new frame, on the
//! interpreter and on the machine code the device's daemon compiled for it.
//!
//! The span measured is a theme module change to the frame drawn from it: the new module is
//! loaded and a button in the theme is drawn into a scene. The compiled path is the daemon's:
//! its compile (outside the session) and then the session's deserialize and paint. The
//! interpreter path is what a session without the daemon's artifact does.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use tilo_theme_sdk::wire;
use tilo_ui::part_paint::PartScene;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::theme_bundle;
use tilo_ui::widget::{Button, Widget as _};
use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect};
use vello::peniko::Color;

const BAUHAUS: &str = include_str!("../../themes/bauhaus/theme.json");

/// A painter that draws a triangle whose apex is `dx` from the centre: a changed module draws a
/// visibly other frame.
fn triangle_module(dx: f64) -> Vec<u8> {
    let mut part = PartScene::new();
    let triangle = {
        let mut path = BezPath::new();
        path.move_to((60.0 + dx, 0.0));
        path.line_to((108.0, 40.0));
        path.line_to((12.0, 40.0));
        path.close_path();
        path
    };
    part.fill(
        triangle,
        tilo_ui::part_paint::Brush::Solid(Color::from_rgb8(0xd0, 0x30, 0x20)),
    );
    let painted = tilo_ui::part_paint::PaintedPart::new(part);
    let params = wire::params_answer(&[
        tilo_ui::part_paint::ParamSpec::new("levels", Some(4.0), 2.0..=16.0),
        tilo_ui::part_paint::ParamSpec::new("depth", Some(0.14), 0.0..=1.0),
        tilo_ui::part_paint::ParamSpec::new("shadow", Some(3.0), 0.0..=10.0),
    ]);
    let answer = wire::paint_answer(Some(&painted));
    let data = |bytes: &[u8]| {
        let mut text = String::new();
        for byte in bytes {
            let _ = write!(text, "\\{byte:02x}");
        }
        text
    };
    let params = data(&params);
    let answer = data(&answer);
    wat::parse_str(format!(
        r#"(module
            (memory (export "memory") 2)
            (data (i32.const 1024) "{params}")
            (data (i32.const 8192) "{answer}")
            (func (export "tilo_theme_buffer") (param i32) (result i32) (i32.const 65536))
            (func (export "tilo_theme_call") (param i32) (result i64)
                (if (result i64) (i32.eq (i32.load8_u (i32.const 65536)) (i32.const 3))
                    (then (i64.or (i64.shl (i64.const 1024) (i64.const 32)) (i64.const {params_len})))
                    (else (i64.or (i64.shl (i64.const 8192) (i64.const 32)) (i64.const {answer_len}))))))"#,
        params_len = params.len() / 3,
        answer_len = answer.len() / 3,
    ))
    .unwrap()
}

fn document(set: &str) -> String {
    BAUHAUS.replace("\"bauhaus\"", &format!("{set:?}"))
}

/// Draw a button in `theme` into a scene: one frame of the theme.
fn frame(theme: &Theme) -> Scene {
    let mut button = Button::new("go", "Go");
    button.apply_layout_external(Rect::new(100.0, 50.0, 220.0, 90.0));
    let mut scene = Scene::new();
    button.draw(&mut tilo_ui::draw::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        tilo_ui::presence::PresenceCtx::default().draw_env(),
    ));
    scene
}

fn median(mut times: Vec<Duration>) -> Duration {
    times.sort_unstable();
    times[times.len() / 2]
}

#[test]
fn a_changed_theme_module_reaches_the_frame_on_the_daemon_s_compiled_code() {
    let first = triangle_module(0.0);
    let changed = triangle_module(6.0);
    let set = "test.aot-change";
    let doc = document(set);

    // Load the first module so the set the theme names exists, as a session's theme does.
    theme_bundle::load(&doc, |_| Ok(Some(first.clone()))).unwrap();

    // Before: a session that translates the changed module itself.
    let mut interpreted = Vec::new();
    let mut first_interpreted = Duration::ZERO;
    for round in 0..7 {
        let module = changed.clone();
        let began = Instant::now();
        let theme = theme_bundle::load(&doc, move |_| Ok(Some(module.clone()))).unwrap();
        let scene = frame(&theme);
        let took = began.elapsed();
        assert!(!scene.encoding().is_empty(), "the frame drew nothing");
        if round == 0 {
            first_interpreted = took;
        }
        interpreted.push(took);
    }

    // The daemon compiles the changed module ahead of time, on its own thread of work, into the
    // store a session reads read-only.
    let dir = tempfile::tempdir().unwrap();
    let cache = tilo_wasm_aot::Cache::at(dir.path()).unwrap();
    let began = Instant::now();
    let _entry = cache.ensure(&changed).unwrap();
    let daemon_took = began.elapsed();

    // After: the session reads the artifact from the device's store, as a process granted the
    // read-only mount does, deserializes it and paints the same frame.
    let mut compiled = Vec::new();
    let mut first_compiled = Duration::ZERO;
    for round in 0..7 {
        let module = changed.clone();
        let store = dir.path().to_owned();
        let began = Instant::now();
        let theme = theme_bundle::load_with(
            &doc,
            move |_| Ok(Some(module.clone())),
            |_, bytes| tilo_theme_painter_wasm::compiled_in(&store, bytes),
        )
        .unwrap();
        let scene = frame(&theme);
        let took = began.elapsed();
        assert!(!scene.encoding().is_empty(), "the frame drew nothing");
        if round == 0 {
            first_compiled = took;
        }
        compiled.push(took);
    }

    // The change shows: the changed module draws a frame, and it is not the first module's.
    let before = frame(&theme_bundle::load(&doc, |_| Ok(Some(first.clone()))).unwrap());
    let after = frame(&theme_bundle::load(&doc, |_| Ok(Some(triangle_module(6.0)))).unwrap());
    assert!(
        !before.encoding_eq(&after),
        "the changed module paints another frame"
    );
    let interpreted_median = median(interpreted);
    let compiled_median = median(compiled);
    println!(
        "\ntheme module change -> frame: interpreted first {first_interpreted:?}, median \
         {interpreted_median:?}\n\
         theme module change -> frame: compiled first {first_compiled:?}, median \
         {compiled_median:?} (daemon compile {daemon_took:?}, outside the session)\n"
    );
}

/// A theme bundle on disk: `theme_bundle::load_from` reads its document and the module of each
/// set it names, and draws a set on the machine code the device's daemon compiled for it when
/// the store it grants read-only holds one. Nothing compiled, and every browser, runs the set
/// on the interpreter.
#[test]
fn a_bundle_on_disk_draws_on_the_store_the_daemon_compiled() {
    use tilo_ui::tokens::SurfaceRole;
    const SET: &str = "test.bundle-disk";
    let bundle = tempfile::tempdir().unwrap();
    let document = BAUHAUS.replace("\"bauhaus\"", &format!("{SET:?}"));
    std::fs::write(bundle.path().join(theme_bundle::DOCUMENT), &document).unwrap();
    let bytes = triangle_module(0.0);
    std::fs::write(bundle.path().join(format!("{SET}.wasm")), &bytes).unwrap();

    // Nothing compiled the set: it runs on the interpreter.
    let store = tempfile::tempdir().unwrap();
    let theme = theme_bundle::load_from(bundle.path(), Some(store.path())).unwrap();
    assert_eq!(theme.finish(SurfaceRole::Control).painter(), SET);
    assert!(!frame(&theme).encoding().is_empty(), "the frame drew nothing");

    // The daemon compiled it into the store the launcher grants read-only: the same bundle now
    // starts the set on that machine code.
    tilo_wasm_aot::Cache::at(store.path())
        .unwrap()
        .ensure(&bytes)
        .unwrap();
    let theme = theme_bundle::load_from(bundle.path(), Some(store.path())).unwrap();
    assert_eq!(theme.finish(SurfaceRole::Control).painter(), SET);
    assert!(!frame(&theme).encoding().is_empty(), "the frame drew nothing");

    // A directory with no document is refused with what it read.
    assert!(matches!(
        theme_bundle::load_from(store.path(), None),
        Err(theme_bundle::BundleError::Read(_))
    ));
}
