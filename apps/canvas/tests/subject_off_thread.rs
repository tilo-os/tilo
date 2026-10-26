//! Selecting a photo's subject through the typed `find-area` door, with the
//! on-device model held mid-inference: the window keeps taking pointer and
//! key input, painting lands as its own step, and the area the model finds
//! is one attributed step once it answers — or is refused when the artwork
//! changed meanwhile.
//!
//! The model gateway here is a stand-in on the launcher's descriptor (the
//! production transport) that answers only when the test lets it, so "the
//! model is still working" is a fact rather than a race. It finds the red
//! of the photo, as a segmentation model finds its subject.
#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "test assertions over exact, hand-written fixtures and the pixels they draw"
)]

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use base64::Engine as _;
use canvas::App;
use serde_json::json;
use tilo_model_client::{Frame, Request, Response, SubjectMatte, read_frame, write_frame};
use ui::prelude::TestApp;
use ui::tilo_actions::{InvocationId, InvocationUpdate};

/// The model gateway, answering each subject request only when told to.
struct HeldModel {
    /// The picture size of each request the model has started on.
    asked: Receiver<(u32, u32)>,
    /// Lets the model answer the request it is working on.
    answer: Sender<()>,
    /// The app's end of the service; its descriptor is what the
    /// environment names.
    _app: UnixStream,
    _env: tilo_test_env::ScopedEnv,
}

impl HeldModel {
    fn start() -> Self {
        let (app, mut service) = UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            tilo_model_client::SERVICE_FD_ENV,
            app.as_raw_fd().to_string(),
        );
        let (asked_by, asked) = channel();
        let (answer, answered) = channel::<()>();
        std::thread::spawn(move || {
            while let Ok(frame) = read_frame::<Frame<Request>>(&mut service) {
                let Request::SegmentSubject { image } = frame.value else {
                    panic!("the canvas asks the gateway only for a subject");
                };
                let jpeg = base64::engine::general_purpose::STANDARD
                    .decode(image.data_base64)
                    .unwrap();
                let picture = image::load_from_memory(&jpeg).unwrap().to_rgb8();
                let (width, height) = picture.dimensions();
                if asked_by.send((width, height)).is_err() || answered.recv().is_err() {
                    return;
                }
                let matte = image::GrayImage::from_fn(width, height, |x, y| {
                    let [red, green, _] = picture.get_pixel(x, y).0;
                    image::Luma([if red > 150 && green < 120 { 255 } else { 0 }])
                });
                let mut png = Vec::new();
                matte
                    .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                    .unwrap();
                let found = SubjectMatte {
                    width,
                    height,
                    matte_png_base64: base64::engine::general_purpose::STANDARD.encode(&png),
                    model_id: "test/red-subject".into(),
                    revision: "0123456789abcdef".into(),
                    load_ms: 0,
                    infer_ms: 0,
                };
                let reply = Response {
                    ok: true,
                    output: Some(serde_json::to_string(&found).unwrap()),
                    configured: None,
                    error: None,
                    model_error: None,
                    event: None,
                };
                if write_frame(&mut service, &reply).is_err() {
                    return;
                }
            }
        });
        Self {
            asked,
            answer,
            _app: app,
            _env: env,
        }
    }

    /// Wait until the model is working on a request: from here until
    /// [`Self::release`], inference has not finished.
    fn working(&self) -> (u32, u32) {
        tilo_test_env::recv("the finder asking the model", &self.asked)
    }

    fn release(&self) {
        self.answer.send(()).unwrap();
    }
}

/// A PNG data: URI of a red disc of radius 60 on white, 200 by 200.
fn disc() -> String {
    let picture = image::RgbaImage::from_fn(200, 200, |x, y| {
        let distance = (f64::from(x) + 0.5 - 100.0).hypot(f64::from(y) + 0.5 - 100.0);
        let cover = (61.0 - distance).clamp(0.0, 2.0) / 2.0;
        let mix = |red: f64, white: f64| (red * cover + white * (1.0 - cover)).round() as u8;
        image::Rgba([mix(200.0, 255.0), mix(30.0, 255.0), mix(30.0, 255.0), 255])
    });
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

/// The app with the photo open and the canvas focused, and where a point
/// of the artwork lies on screen.
fn open() -> (TestApp<App>, impl Fn(f64, f64) -> (f64, f64)) {
    let mut app = TestApp::<App>::new().unwrap();
    app.call(
        "draw",
        json!({"marks": [{"id": "photo", "shape": {"kind": "image", "src": disc(), "x": 0, "y": 0, "width": 200, "height": 200}}]}),
    )
    .unwrap();
    let canvas = app.harness().targets().get("canvas").copied().unwrap();
    app.harness()
        .perform_named("canvas", &ui::a11y::Action::Focus)
        .unwrap();
    app.harness().key("v");
    // The artwork is fitted into the canvas less its 24-point padding.
    let art = app.state();
    let zoom = ((canvas.width() - 48.0) / art.width).min((canvas.height() - 48.0) / art.height);
    let origin = (
        canvas.center().x - art.width * zoom / 2.0,
        canvas.center().y - art.height * zoom / 2.0,
    );
    (app, move |x, y| (origin.0 + x * zoom, origin.1 + y * zoom))
}

fn find_subject() -> canvas::resource::ViewOp {
    canvas::resource::ViewOp::FindArea {
        area: canvas::resource::FindArea {
            subject: Some(true),
            ..Default::default()
        },
    }
}

fn finished(app: &mut TestApp<App>, invocation: InvocationId) -> bool {
    app.harness()
        .job_updates(invocation)
        .iter()
        .any(InvocationUpdate::is_terminal)
}

fn settle(app: &mut TestApp<App>, invocation: InvocationId) -> Vec<InvocationUpdate> {
    assert!(
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), |harness| {
                harness
                    .job_updates(invocation)
                    .iter()
                    .any(InvocationUpdate::is_terminal)
            }),
        "the finder answered once the model did"
    );
    app.harness().job_updates(invocation).to_vec()
}

fn millis(elapsed: Duration) -> f64 {
    elapsed.as_secs_f64() * 1000.0
}

#[test]
fn the_window_takes_input_while_the_subject_model_works() {
    let model = HeldModel::start();
    let (mut app, at) = open();
    let (x, y) = at(100.0, 100.0);
    // One round of ordinary input: select the photo with a click, and let
    // it go with Escape. Timed once with nothing running, for comparison.
    let round = |app: &mut TestApp<App>| {
        let begun = Instant::now();
        app.harness().click_at(x, y);
        app.harness().key("Escape");
        begun.elapsed()
    };
    let idle = round(&mut app);
    let before = app.state();
    let versions = app.versions().len();

    let begun = Instant::now();
    let invocation = app.start_effect(&find_subject()).unwrap();
    let dispatched = begun.elapsed();
    // The model has the picture and will not answer until released: the
    // dispatch above returned without waiting for inference.
    let (width, height) = model.working();
    assert!(width > 0 && height > 0);
    assert!(!finished(&mut app, invocation));

    // The window says what it is doing, and keeps answering the pointer:
    // clicking the photo with the select tool selects it.
    let finding = ui::i18n::I18nManager::lookup_plain("canvas-finding-subject");
    let says = |harness: &ui::headless::FrameHarness, what: &str| {
        harness.inspection_json().unwrap().contains(what)
    };
    // What the canvas is doing is said in the window's bar, where a transient
    // notice belongs: the row it used to stand in under the title is the
    // bar's now.
    let says_it_is_finding = |_: &ui::headless::FrameHarness| {
        ui::window_toolbar::shown()
            .and_then(|toolbar| toolbar.notice)
            .is_some_and(|notice| notice.text == finding)
    };
    assert!(
        app.harness()
            .wait_until(tilo_test_env::hang_bound(), says_it_is_finding),
        "the window bar says {finding:?}"
    );
    assert!(
        says(app.harness(), "0 selected;"),
        "nothing is selected yet"
    );
    let held = round(&mut app);
    app.harness().click_at(x, y);
    assert!(
        says(app.harness(), "1 selected;"),
        "the click selected the photo while the model worked"
    );
    assert!(
        !finished(&mut app, invocation),
        "the model is still holding its answer"
    );
    assert_eq!(app.state(), before, "nothing was found yet");
    assert_eq!(app.versions().len(), versions);

    model.release();
    let updates = settle(&mut app, invocation);
    let output = updates
        .iter()
        .find_map(|update| match update {
            InvocationUpdate::Succeeded { output, .. } => output.value.clone(),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{updates:?}"));
    assert!(
        output["found_by"]["model"]
            .as_str()
            .is_some_and(|model| model.starts_with("test/red-subject@")),
        "{output}"
    );
    let [left, top, wide, high] = [0, 1, 2, 3].map(|at| {
        output["selection"]["bounds"][at]
            .as_f64()
            .unwrap_or_else(|| panic!("{output}"))
    });
    // The disc runs from 39 to 161.
    assert!(
        (left - 39.0).abs() < 4.0 && (top - 39.0).abs() < 4.0,
        "{output}"
    );
    assert!(
        (wide - 122.0).abs() < 6.0 && (high - 122.0).abs() < 6.0,
        "{output}"
    );
    assert!(
        !says(app.harness(), &finding),
        "the header no longer says it is finding"
    );
    assert_eq!(
        app.versions().len(),
        versions.saturating_add(1),
        "one step of history"
    );
    assert!(app.undo());
    assert_eq!(app.state(), before);
    eprintln!(
        "find-area subject: dispatch {:.2}ms on the window's thread; a click and Escape {:.2}ms idle, {:.2}ms with the model held",
        millis(dispatched),
        millis(idle),
        millis(held)
    );
}

#[test]
fn painting_while_the_subject_model_works_lands_and_the_stale_area_is_refused() {
    let model = HeldModel::start();
    let (mut app, at) = open();
    app.harness().key("b");
    // A stroke with nothing running, for comparison.
    let painted = Instant::now();
    app.harness().drag_at(at(20.0, 20.0), at(180.0, 20.0));
    let idle = painted.elapsed();
    let marks = app.state().marks.len();
    let invocation = app.start_effect(&find_subject()).unwrap();
    model.working();

    // A brush stroke across the photo lands while the model works.
    let painted = Instant::now();
    app.harness().drag_at(at(20.0, 180.0), at(180.0, 180.0));
    let held = painted.elapsed();
    let edited = app.state();
    assert_eq!(
        edited.marks.len(),
        marks.saturating_add(1),
        "the stroke landed"
    );
    assert!(!finished(&mut app, invocation));

    // The model answers for the picture before the stroke: that area is not
    // laid over the newer artwork.
    model.release();
    let updates = settle(&mut app, invocation);
    assert!(
        updates.iter().any(|update| matches!(update,
            InvocationUpdate::Failed { error, .. } if error.message.contains("changed"))),
        "{updates:?}"
    );
    assert_eq!(app.state(), edited);
    assert!(app.undo(), "the stroke is its own step");
    assert_eq!(app.state().marks.len(), marks);
    assert!(app.redo());

    // The refusal released the finder: asking again finds the subject.
    let again = app.start_effect(&find_subject()).unwrap();
    model.working();
    model.release();
    let updates = settle(&mut app, again);
    assert!(
        updates
            .iter()
            .any(|update| matches!(update, InvocationUpdate::Succeeded { .. })),
        "{updates:?}"
    );
    assert!(app.state().selection.is_some());
    assert_eq!(app.state().marks.len(), marks.saturating_add(1));
    eprintln!(
        "find-area subject: a brush stroke {:.2}ms idle, {:.2}ms with the model held",
        millis(idle),
        millis(held)
    );
}
