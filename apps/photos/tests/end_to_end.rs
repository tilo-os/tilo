//! The scenario, end to end: open a photograph, adjust it, get the result out.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Arc, Mutex};

use ui::clipboard::{ClipboardBackend, ClipboardTarget};
use ui::headless::{Framed, viewport};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::Rect;

use automerge::{AutoCommit, ROOT};
use photos::model::{Album, Edits, PhotoLibrary, Rect01};
use photos::pixels::{self, Rgba};
use photos::{PhotosApp, shots_pattern};
use ui::tilo_resource::{Fields as _, Resource as _};

fn new_state() -> ui::State {
    let mut doc = AutoCommit::new();
    PhotoLibrary::initialize(&mut doc).expect("a new library initializes");
    ui::State::from_doc(doc)
}

fn library(doc: &AutoCommit) -> PhotoLibrary {
    PhotoLibrary::read(doc).expect("the library reads")
}

fn set_library(doc: &mut AutoCommit, library: &PhotoLibrary) {
    library
        .write_fields(doc, &ROOT)
        .expect("the library writes");
}

fn published(doc: &AutoCommit) -> tilo_actions::ResourceSpec {
    ui::tilo_resource::publish::<PhotoLibrary>("photos.library", "Photos", &library(doc))
}

struct Harness {
    dispatch: ui::framework::Dispatcher<PhotoLibrary>,
    unmounted: Option<(PhotosApp, ui::State)>,
    mounted: Option<Framed<PhotosApp>>,
    theme: Theme,
    size: (f32, f32),
}

impl Harness {
    fn new(w: f32, h: f32) -> Self {
        let dispatch = ui::framework::testing::dispatcher("so.tilo.photos");
        Self {
            unmounted: Some((
                PhotosApp::new().with_dispatcher(dispatch.clone()),
                new_state(),
            )),
            dispatch,
            mounted: None,
            theme: Theme::dark(),
            size: (w, h),
        }
    }

    fn parts(&mut self) -> (&mut PhotosApp, &mut ui::State) {
        let (app, state) = if let Some(framed) = self.mounted.as_mut() {
            Framed::split(framed)
        } else {
            let (app, state) = self.unmounted.as_mut().expect("harness owns its app");
            (app, state)
        };
        ui::framework::testing::resolve(&self.dispatch, &mut state.doc)
            .expect("submitted edits apply");
        let library = library(&state.doc);
        app.state().adopt_resource_state(&std::rc::Rc::new(library));
        (app, state)
    }

    fn app(&mut self) -> &mut PhotosApp {
        self.parts().0
    }

    fn state(&mut self) -> &mut ui::State {
        self.parts().1
    }

    fn frame(&mut self) -> Scene {
        let _ = self.parts();
        let viewport = viewport(self.size);
        if let Some(framed) = self.mounted.as_mut() {
            Framed::harness_mut(framed)
                .relayout_themed(viewport, self.theme.clone())
                .expect("photos frame lays out");
        } else {
            let (app, state) = self.unmounted.take().expect("harness owns its app");
            self.mounted = Some(Framed::new(state, app, viewport, self.theme.clone()));
        }
        let framed = self.mounted.as_ref().expect("mounted above");
        Framed::harness(framed).scene().clone()
    }

    fn frame_until_shown(&mut self) -> Scene {
        let deadline = tilo_test_env::hang_deadline();
        let mut scene = self.frame();
        while !self.app().state().has_image() {
            assert!(
                ui::time::now() < deadline,
                "the photograph did not decode off the frame in time"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
            scene = self.frame();
        }
        scene
    }

    fn edits(&mut self) -> Edits {
        let id = self
            .app()
            .state()
            .open_photo_id()
            .expect("a photograph is open")
            .to_owned();
        library(&self.state().doc)
            .photo(&id)
            .expect("the shown photograph is in the library")
            .edits
    }

    fn set_edits(&mut self, edits: Edits) {
        let id = self
            .app()
            .state()
            .open_photo_id()
            .expect("a photograph is open")
            .to_owned();
        let doc = &mut self.state().doc;
        let mut library = library(doc);
        library
            .photo_mut(&id)
            .expect("the photograph is there")
            .edits = edits;
        set_library(doc, &library);
    }

    fn window_rect(&self) -> Rect {
        Rect::new(0.0, 0.0, f64::from(self.size.0), f64::from(self.size.1))
    }
}

fn staged(dir: &std::path::Path, w: u32, h: u32) -> (String, Rgba) {
    let img = shots_pattern(w, h);
    let path = dir.join("harbour.png");
    std::fs::write(&path, pixels::encode_png(&img).unwrap()).unwrap();
    (path.to_string_lossy().into_owned(), img)
}

#[test]
fn library_accessibility_is_certified_and_album_actions_are_canonical() {
    eprintln!("phase populated_large_text_library_passes_native_accessibility_certification");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);
        let mut state = new_state();
        let dispatch = ui::framework::testing::dispatcher("so.tilo.photos");
        let app = PhotosApp::new().with_dispatcher(dispatch.clone());
        app.state().open_path(&path);
        ui::framework::testing::resolve(&dispatch, &mut state.doc).expect("the photograph imports");
        app.state().show_library();
        let theme = Theme::high_contrast()
            .with_text_scale(2.0)
            .with_reduce_motion(true);
        let mut driver =
            ui::headless::FrameHarness::new_with_theme(state, Box::new(app), (390, 844), theme);
        PhotoLibrary::register(driver.action_registry_mut());
        let audit = driver.native_app_audit();
        assert!(
            audit.is_certified(),
            "populated large-text library native audit: native={:?}, accessibility={:?}",
            audit.issues,
            audit.accessibility.issues
        );
        assert!(driver.accessibility_audit().is_clean());
    }

    eprintln!("phase destination_album_chooser_keeps_native_actions_unique_and_canonical");
    {
        fn collect_targets(node: &ui::a11y::AccessNode, targets: &mut Vec<String>) {
            if let Some(target) = &node.action_target {
                targets.push(target.clone());
            }
            for child in &node.children {
                collect_targets(child, targets);
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 320, 240);
        let mut hz = Harness::new(960.0, 700.0);
        hz.app().state().open_path(&path);
        let photo_id = library(&hz.state().doc)
            .photos
            .values()
            .next()
            .unwrap()
            .id
            .clone();
        let mut library = library(&hz.state().doc);
        library.add_album(Album {
            id: "photos.album:destination-a".into(),
            name: "Weekend plans".into(),
            photo_ids: photos::model::labels([photo_id.clone()]),
        });
        library.add_album(Album {
            id: "photos.album:destination-b".into(),
            name: "Home projects".into(),
            photo_ids: photos::model::labels([photo_id]),
        });
        set_library(&mut hz.state().doc, &library);
        let (app, _) = hz.parts();
        app.state().show_selection_for_shot();
        app.state().show_selection_destination_chooser_for_shot();
        hz.frame_until_shown();

        let tree = hz
            .app()
            .accessibility()
            .expect("Photos publishes accessibility");
        let mut targets = Vec::new();
        collect_targets(&tree, &mut targets);
        for target in [
            "photos:selection:destination:photos.album:destination-a",
            "photos:selection:destination:photos.album:destination-b",
            "photos:selection:destination-cancel",
            "photos:selection:destination-close",
        ] {
            assert_eq!(
                targets
                    .iter()
                    .filter(|candidate| candidate.as_str() == target)
                    .count(),
                1,
                "chooser action `{target}` must be present exactly once: {targets:?}"
            );
        }
        assert!(
            !targets
                .iter()
                .any(|target| target == "photos:selection:destination-apply"),
            "Use album must remain unavailable until a destination is selected"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn photographs_fit_the_canvas_and_non_images_open_nothing() {
    eprintln!("phase the_whole_photograph_is_inside_the_canvas_at_every_window_size");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 2400, 1600);

        for (w, h) in [
            (1280.0, 800.0),
            (640.0, 480.0),
            (400.0, 900.0),  // taller than wide: the photograph is not
            (1600.0, 300.0), // a letterbox strip
            (320.0, 240.0),  // small enough that chrome dominates
        ] {
            let mut hz = Harness::new(w, h);
            hz.app().state().open_path(&path);
            hz.frame_until_shown();

            assert!(
                hz.app().state().has_image(),
                "{w}x{h}: the photograph did not load"
            );

            let window = hz.window_rect();
            let canvas = hz.app().state().canvas_rect();
            let status = hz.app().state().status_rect();

            for (name, r) in [("canvas", canvas), ("status", status)] {
                assert!(
                    r.x0 >= window.x0 - 0.5
                        && r.y0 >= window.y0 - 0.5
                        && r.x1 <= window.x1 + 0.5
                        && r.y1 <= window.y1 + 0.5,
                    "{w}x{h}: the {name} ({r:?}) escapes the window ({window:?})"
                );
            }
            assert!(
                canvas.width() > 0.0 && canvas.height() > 0.0,
                "{w}x{h}: the canvas was squeezed to nothing: {canvas:?}"
            );

            let visible = hz.app().state().visible_rect();
            let (x, y, vw, vh) = (visible.x, visible.y, visible.w, visible.h);
            assert!(
                x.abs() < 1e-6 && y.abs() < 1e-6,
                "{w}x{h}: the photograph starts clipped at ({x}, {y})"
            );
            assert!(
                (vw - 1.0).abs() < 1e-6 && (vh - 1.0).abs() < 1e-6,
                "{w}x{h}: only {:.1}% x {:.1}% of the photograph is on screen",
                vw * 100.0,
                vh * 100.0
            );
        }
    }

    eprintln!("phase a_portrait_photograph_initially_fits_a_narrow_portrait_window");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 800, 1200);
        let mut hz = Harness::new(390.0, 844.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let visible = hz.app().state().visible_rect();
        assert!(
            visible.is_full(),
            "a newly opened portrait photograph was clipped instead of fitted: {visible:?}"
        );
    }

    eprintln!("phase a_loaded_photograph_produces_a_non_empty_scene");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 300, 200);
        let mut hz = Harness::new(1000.0, 700.0);

        let images = |scene: &Scene| {
            scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == vello_encoding::DrawTag::IMAGE)
                .count()
        };

        let empty = hz.frame();
        assert_eq!(
            images(&empty),
            0,
            "an empty app drew an image from somewhere"
        );

        hz.app().state().open_path(&path);
        let loaded = hz.frame_until_shown();
        assert_eq!(
            images(&loaded),
            1,
            "the photograph was not drawn into the scene"
        );
    }

    eprintln!("phase nothing_open_publishes_open_and_still_draws");
    {
        let mut hz = Harness::new(900.0, 600.0);
        let scene = hz.frame();
        let resources = [published(&hz.state().doc)];
        assert_eq!(resources[0].id.0, "photos.library");
        assert!(
            <photos::actions::PhotosEffect as ui::tilo_resource::Effect>::specs()
                .iter()
                .any(|action| action.id == "view-library"),
            "the empty app must remain driveable through its library effect"
        );
        assert!(
            scene.encoding().n_paths > 0,
            "the empty state drew nothing at all"
        );
        assert!(!hz.app().state().has_image());
    }

    eprintln!("phase opening_a_non_image_says_so_and_opens_nothing");
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"not a picture").unwrap();

        let mut hz = Harness::new(900.0, 600.0);
        hz.app().state().open_path(&path.to_string_lossy());
        hz.frame();

        assert!(!hz.app().state().has_image());
        assert!(hz.app().state().open_photo_id().is_none());
        assert!(library(&hz.state().doc).photos.is_empty());
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fx/fy are literal fractions in 0..=1 in this test, so (dim - 1) * frac stays within 0..dim"
)]
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn rotate_crop_and_reset_change_exactly_the_exported_pixels() {
    eprintln!("phase rotating_changes_the_exported_pixels");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 300, 200);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let before = hz.app().state().edited_pixels().expect("rendered").clone();
        assert_eq!((before.w, before.h), (300, 200));

        let turned = hz.edits().rotated_right();
        hz.set_edits(turned);
        hz.frame();

        let after = hz
            .app()
            .state()
            .edited_pixels()
            .expect("re-rendered")
            .clone();
        assert_eq!((after.w, after.h), (200, 300), "the axes did not swap");
        assert_ne!(after.bytes, before.bytes, "the buffer is unchanged");

        let (sw, sh) = (before.w, before.h);
        let mut checked = 0;
        for (sx, sy) in [(0, 0), (sw - 1, 0), (0, sh - 1), (sw - 1, sh - 1), (37, 61)] {
            let want = before.pixel(sx, sy).unwrap();
            let got = after.pixel(sh - 1 - sy, sx).unwrap();
            assert_eq!(
                got,
                want,
                "source ({sx}, {sy}) did not land at ({}, {sx}) after a clockwise turn",
                sh - 1 - sy
            );
            checked += 1;
        }
        assert_eq!(checked, 5);

        let brightness = |img: &Rgba, fx: f64, fy: f64| {
            let p = img
                .pixel(
                    ((f64::from(img.w) - 1.0) * fx) as u32,
                    ((f64::from(img.h) - 1.0) * fy) as u32,
                )
                .unwrap();
            u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2])
        };
        assert!(
            brightness(&after, 0.72, 0.28) > brightness(&after, 0.28, 0.72),
            "the sun is not where a clockwise turn puts it"
        );
    }

    eprintln!("phase cropping_changes_the_exported_pixels");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 400);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let before = hz.app().state().edited_pixels().expect("rendered").clone();
        assert_eq!((before.w, before.h), (400, 400));

        let e = Edits::default().cropped_from_view(Rect01 {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();

        let after = hz
            .app()
            .state()
            .edited_pixels()
            .expect("re-rendered")
            .clone();
        assert_eq!((after.w, after.h), (200, 200), "the crop did not resize");
        assert!(
            after.bytes.len() < before.bytes.len(),
            "the cropped buffer is not smaller"
        );
        assert_eq!(
            after.pixel(0, 0).unwrap(),
            original.pixel(200, 200).unwrap()
        );
        assert_eq!(
            after.pixel(5, 7).unwrap(),
            original.pixel(205, 207).unwrap()
        );
    }

    eprintln!("phase reset_restores_the_original_pixels_exactly");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 240, 160);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
            x: 0.1,
            y: 0.1,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();
        assert_ne!(
            hz.app().state().edited_pixels().unwrap().bytes,
            original.bytes
        );

        hz.set_edits(Edits::default());
        hz.frame();
        assert_eq!(
            hz.app().state().edited_pixels().unwrap().bytes,
            original.bytes,
            "reset did not restore the original"
        );
    }

    eprintln!("phase cropping_to_the_view_keeps_what_the_viewport_frames");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 400, 400);
        let mut hz = Harness::new(600.0, 600.0 + 48.0 + 28.8);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        hz.app().state().set_zoom(2.0);
        hz.frame();

        let view = hz.app().state().visible_rect();
        assert!(
            (view.w - 0.5).abs() < 0.05,
            "a 2x zoom should frame about half the width, got {view:?}"
        );

        let e = hz.edits().cropped_from_view(view);
        hz.set_edits(e);
        hz.frame();

        let after = hz
            .app()
            .state()
            .edited_pixels()
            .expect("re-rendered")
            .clone();
        assert!(
            after.w < 400 && after.h < 400,
            "cropping to a 2x view did not shrink the image: {}x{}",
            after.w,
            after.h
        );
        assert!(
            (i64::from(after.w) - 200).abs() <= 12,
            "expected about half the width, got {}",
            after.w
        );
    }
}

#[derive(Default)]
struct RecordingClipboard {
    image: Mutex<Option<(usize, usize, Vec<u8>)>>,
}

impl ClipboardBackend for RecordingClipboard {
    fn set_text(&self, _t: ClipboardTarget, _s: &str) -> bool {
        true
    }
    fn text(&self, _t: ClipboardTarget) -> Option<String> {
        None
    }
    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        *self.image.lock().unwrap() = Some((width, height, rgba.to_vec()));
        true
    }
    fn name(&self) -> &'static str {
        "recording-test"
    }
}

#[test]
fn copy_and_save_export_what_was_shown_without_overwriting() {
    eprintln!("phase copy_puts_the_edited_pixels_on_the_clipboard");
    {
        let recorder = Arc::new(RecordingClipboard::default());
        let services = ui::host::HostServices::null().with_clipboard(recorder.clone());
        let _host = ui::host::enter(&services);

        {
            let mut empty = Harness::new(900.0, 600.0);
            empty.frame();
            assert!(
                !empty.app().state().copy_to_clipboard(),
                "copied with nothing open"
            );
            assert!(
                recorder.image.lock().unwrap().is_none(),
                "a refused copy still offered something"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 300);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();

        let shown = hz.app().state().edited_pixels().expect("rendered").clone();
        assert!(hz.app().state().copy_to_clipboard(), "the copy was refused");

        let got = recorder
            .image
            .lock()
            .unwrap()
            .clone()
            .expect("nothing was copied");
        assert_eq!(
            (got.0, got.1),
            (shown.w as usize, shown.h as usize),
            "the clipboard got a different size from what is on screen"
        );
        assert_eq!(got.2, shown.bytes, "the clipboard got different pixels");
        assert_ne!(
            got.2, original.bytes,
            "the clipboard got the unedited original"
        );
        assert_eq!(
            got.2.len(),
            got.0 * got.1 * 4,
            "the buffer does not match its declared size"
        );
    }

    eprintln!("phase save_writes_a_png_that_decodes_to_what_was_shown");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 320, 240);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let turned = hz.edits().rotated_left();
        hz.set_edits(turned);
        hz.frame();
        let shown = hz.app().state().edited_pixels().expect("rendered").clone();

        let out = dir.path().join("out.png");
        assert!(
            hz.app()
                .state()
                .save_to(&ui::tilo_resource::AbsolutePath::new(out.clone()).unwrap()),
            "save failed"
        );

        let written = std::fs::read(&out).unwrap();
        assert_eq!(&written[1..4], b"PNG", "that is not a PNG");
        let back = pixels::decode(&written).expect("the saved file does not decode");
        assert_eq!((back.w, back.h), (shown.w, shown.h));
        assert_eq!(
            back.bytes, shown.bytes,
            "the saved pixels differ from the shown ones"
        );
    }
}

#[test]
fn save_asks_the_chooser_with_a_bare_name() {
    eprintln!("phase save_suggests_a_bare_name_that_does_not_overwrite_the_original");
    for name in [
        "harbour.png",
        "/var/tmp/tilo/home/user/harbour.png",
        "../harbour.png",
        "",
    ] {
        let open = photos::OpenImage::new(name, 10, 10, ui::tilo_resource::Bytes::of(b"a picture"));
        let suggested = photos::app::Photos::suggested_save_name(&open);
        assert_ne!(suggested, open.name);
        let path = std::path::Path::new(&suggested);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(suggested.as_str()),
            "the suggestion is a bare file name, got {suggested}"
        );
        assert!(suggested.ends_with("-edited.png"), "got {suggested}");
    }

    eprintln!("phase save_without_a_path_asks_the_chooser_and_writes_where_it_answers");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 320, 240);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();
        let shown = hz.app().state().edited_pixels().expect("rendered").clone();

        let chosen = dir.path().join("chosen.png");
        let (services, recorder) = ui::host::HostServices::recording();
        recorder.respond_to_pick(ui::portal::PortalOutcome::Opened(chosen.clone()));
        let _host = ui::host::enter(&services);
        assert!(
            hz.app().state().save_with_chooser(),
            "nothing was offered to save"
        );
        let requests = recorder.pick_requests();
        assert_eq!(requests.len(), 1, "Save asks the chooser once");
        assert_eq!(requests[0].suggested_name(), Some("harbour-edited.png"));
        tilo_test_env::wait_until("the chooser's save to finish", || {
            hz.frame();
            !hz.app().state().is_saving()
        });
        let back = pixels::decode(&std::fs::read(&chosen).unwrap()).expect("a PNG");
        assert_eq!(
            back.bytes, shown.bytes,
            "the saved pixels differ from the shown ones"
        );
        assert!(
            !std::path::Path::new("harbour-edited.png").exists(),
            "Save wrote into the working directory"
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn resource_surface_acts_on_the_open_photograph_without_a_display() {
    eprintln!("phase the_resource_surface_enumerates_the_open_photograph_and_acts_on_it");
    {
        use tilo_actions::{ActionRegistry, flatten};

        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 400, 200);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let before = hz.state().doc.save();
        let resources = [published(&hz.state().doc)];
        let flat = flatten(&resources);
        assert_eq!(
            hz.state().doc.save(),
            before,
            "enumeration mutated the document"
        );

        let shown = hz
            .app()
            .state()
            .open_photo_id()
            .expect("the photograph is open")
            .to_owned();
        let photo = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == photos::resource::photo_resource_id(&shown))
            .expect("the open photograph is published in the library");
        assert_eq!(photo.label, "harbour.png");
        let content =
            PhotoLibrary::read_content(&library(&hz.state().doc), &hz.state().doc, Some(&shown))
                .expect("a photograph reads as content");
        assert_eq!(
            (
                content["image"]["width"].as_u64(),
                content["image"]["height"].as_u64()
            ),
            (Some(400), Some(200))
        );

        for row in &flat {
            assert!(!row.key.is_empty());
            assert!(
                !row.key.contains("px") && !row.key.chars().any(|c| c == '@'),
                "an action key looks positional: {}",
                row.key
            );
        }

        let mut registry = ActionRegistry::new();
        PhotoLibrary::register(&mut registry);
        let spec = photo
            .actions
            .iter()
            .find(|a| a.id == "rotate-right")
            .expect("rotate-right is published");
        let encoded = spec
            .build_json(&serde_json::Map::new())
            .expect("rotate-right needs nothing but its photograph");
        registry
            .decode(&encoded)
            .unwrap()
            .apply(&mut hz.state().doc)
            .unwrap();
        hz.frame();

        let after = hz
            .app()
            .state()
            .edited_pixels()
            .expect("re-rendered")
            .clone();
        assert_eq!(
            (after.w, after.h),
            (200, 400),
            "invoking the published action did not rotate the pixels"
        );
    }

    eprintln!("phase a_caller_with_no_display_crops_by_address_and_gets_those_pixels");
    {
        use tilo_actions::ActionRegistry;

        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 400);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();

        let resources = [published(&hz.state().doc)];
        let id = hz
            .app()
            .state()
            .open_photo_id()
            .expect("the harness opened one")
            .to_owned();
        assert_eq!(
            photos::actions::content_of(&id),
            Some(tilo_content::hash_bytes(&std::fs::read(&path).unwrap()).as_str()),
            "the published id is not the one derived from the photograph"
        );
        let photo = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == photos::resource::photo_resource_id(&id))
            .expect("the photograph is published by its id");

        let crop = photo
            .actions
            .iter()
            .find(|a| a.id == "crop")
            .expect("crop is published");
        assert!(
            crop.params().iter().any(|param| param.name == "region"),
            "crop collects the region"
        );
        let encoded = crop
            .build_json(
                serde_json::json!({ "region": { "x": 0.5, "y": 0.5, "w": 0.5, "h": 0.5 } })
                    .as_object()
                    .unwrap(),
            )
            .expect("the region binds");

        let mut registry = ActionRegistry::new();
        PhotoLibrary::register(&mut registry);
        registry
            .decode(&encoded)
            .expect("the envelope decodes")
            .apply(&mut hz.state().doc)
            .expect("the crop applies");
        hz.frame();

        let after = hz
            .app()
            .state()
            .edited_pixels()
            .expect("re-rendered")
            .clone();
        assert_eq!(
            (after.w, after.h),
            (200, 200),
            "cropping by address did not resize the exported pixels"
        );
        assert_eq!(
            after.pixel(0, 0).unwrap(),
            original.pixel(200, 200).unwrap()
        );
        assert_eq!(
            after.pixel(9, 3).unwrap(),
            original.pixel(209, 203).unwrap()
        );

        let saved = hz.state().doc.save().len();
        assert!(
            saved < 4096,
            "the document grew to {saved} bytes after a crop by address"
        );
    }
}

#[allow(
    clippy::cast_precision_loss,
    reason = "label.chars().count() is a handful of glyphs in a toolbar tooltip, nowhere near f32's precision limit"
)]
#[test]
fn toolbar_controls_fit_legibly_at_every_size_and_theme() {
    eprintln!("phase every_toolbar_control_is_inside_the_window_at_every_size");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);

        for (w, h) in [
            (1600.0, 900.0),
            (1280.0, 800.0),
            (960.0, 700.0), // half of a 1920 display
            (800.0, 600.0), // half of a 1600 display
            (720.0, 560.0), // where Save used to vanish
            (480.0, 640.0),
            (360.0, 520.0),
        ] {
            let mut hz = Harness::new(w, h);
            hz.app().state().open_path(&path);
            hz.frame_until_shown();
            hz.frame();

            let names = hz.app().state().window_bar_names();
            for wanted in ["Library", "Open", "Save"] {
                assert!(
                    names.iter().any(|name| name == wanted),
                    "{w}x{h}: the window's bar lost {wanted}: {names:?}"
                );
            }
        }
    }

    eprintln!("phase an_empty_app_reserves_no_status_band");
    {
        let mut hz = Harness::new(1000.0, 700.0);
        hz.frame();

        assert!(!hz.app().state().has_image(), "nothing should be open");
        assert_eq!(
            hz.app().state().status(),
            "",
            "an app with no photograph open has nothing to report"
        );
        let status = hz.app().state().status_rect();
        assert_eq!(
            status.height(),
            0.0,
            "an empty status line still reserved a band: {status:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 200, 150);
        hz.app().state().open_path(&path);
        hz.frame_until_shown();
        assert!(
            hz.app().state().status_rect().height() > 0.0,
            "an open photograph must get a status line"
        );
        assert!(
            hz.app().state().status().contains("200"),
            "got `{}`",
            hz.app().state().status()
        );
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn library_cards_lay_out_inside_the_canvas_at_every_size() {
    eprintln!("phase library_card_controls_stay_inside_their_cards_at_wide_and_narrow_sizes");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);
        for (w, h) in [(1280.0, 800.0), (390.0, 844.0)] {
            let mut hz = Harness::new(w, h);
            hz.app().state().open_path(&path);
            let mut library = library(&hz.state().doc);
            let seed = library.photos.values().next().unwrap().clone();
            for suffix in ["dusk", "trail", "museum", "market"] {
                let mut extra = seed.clone();
                extra.id = format!("{}:{suffix}", seed.id);
                extra.image.name = format!("{suffix}-photograph-with-a-long-name.png");
                extra.tags = photos::model::labels([
                    "a deliberately long but bounded metadata label".to_owned(),
                ]);
                library.add_photo(extra);
            }
            set_library(&mut hz.state().doc, &library);
            hz.app().state().show_library();
            hz.frame_until_shown();
            hz.frame();

            let layouts = hz.app().state().library_card_action_rects();
            assert!(
                !layouts.is_empty(),
                "{w}x{h}: no library cards were measured"
            );
            for (card, details, remove, favorite) in &layouts {
                for (name, control) in [
                    ("details", details),
                    ("remove", remove),
                    ("favorite", favorite),
                ] {
                    assert!(
                        control.x0 >= card.x0
                            && control.y0 >= card.y0
                            && control.x1 <= card.x1
                            && control.y1 <= card.y1,
                        "{w}x{h}: {name} control {control:?} escapes card {card:?}"
                    );
                    assert!(
                        control.height() >= 32.0 - f64::EPSILON,
                        "{w}x{h}: {name} is below the pointer target minimum: {control:?}"
                    );
                }
            }
            for (index, (card, ..)) in layouts.iter().enumerate() {
                for (other, ..) in layouts.iter().skip(index + 1) {
                    assert!(
                        card.x1 <= other.x0
                            || other.x1 <= card.x0
                            || card.y1 <= other.y0
                            || other.y1 <= card.y0,
                        "{w}x{h}: neighboring cards overlap: {card:?} and {other:?}"
                    );
                }
            }
        }
    }

    eprintln!("phase desktop_library_uses_two_complete_rows_before_leaving_canvas_space");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);
        let mut hz = Harness::new(1440.0, 900.0);
        hz.app().state().open_path(&path);
        let mut library = library(&hz.state().doc);
        let seed = library.photos.values().next().unwrap().clone();
        for suffix in ["dusk", "trail", "museum", "market"] {
            let mut extra = seed.clone();
            extra.id = format!("{}:{suffix}", seed.id);
            extra.image.name = format!("{suffix}.png");
            library.add_photo(extra);
        }
        library.add_album(Album {
            id: "photos.album:layout".into(),
            name: "Layout coverage".into(),
            photo_ids: library.all_photos().map(|photo| photo.id.clone()).collect(),
        });
        set_library(&mut hz.state().doc, &library);
        hz.app().state().show_library();
        hz.frame_until_shown();
        hz.frame();

        let cards = hz.app().state().library_card_action_rects();
        assert_eq!(
            cards.len(),
            5,
            "every photo of a five-photo desktop library is on screen"
        );
        assert!(
            cards.iter().all(|(card, ..)| card.width() >= 160.0),
            "the desktop timeline shows photos at a useful size: {cards:?}"
        );
        assert!(
            cards.iter().all(|(card, ..)| card.y1 <= 900.0),
            "every rendered desktop card must remain fully pointer and AT reachable: {cards:?}"
        );
    }

    eprintln!("phase large_text_library_keeps_toolbar_and_card_actions_separate");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);
        let mut hz = Harness::new(1440.0, 900.0);
        hz.theme = Theme::high_contrast().with_text_scale(2.0);
        hz.app().state().open_path(&path);
        let mut library = library(&hz.state().doc);
        let seed = library.photos.values().next().unwrap().clone();
        for suffix in ["dusk", "trail", "museum", "market"] {
            let mut extra = seed.clone();
            extra.id = format!("{}:{suffix}", seed.id);
            extra.image.name = format!("{suffix}-photograph-with-a-long-name.png");
            extra.tags =
                photos::model::labels(
                    ["a deliberately long but bounded metadata label".to_owned()],
                );
            library.add_photo(extra);
        }
        set_library(&mut hz.state().doc, &library);
        hz.app().state().show_library();
        hz.frame_until_shown();
        hz.frame();

        let names = hz.app().state().window_bar_names();
        assert!(
            names.len() >= 3,
            "large text keeps the window bar's commands: {names:?}"
        );
        let cards = hz.app().state().library_card_action_rects();
        assert!(!cards.is_empty(), "large text keeps photos on screen");
        for (card, details, remove, favorite) in &cards {
            for control in [details, remove, favorite] {
                assert!(
                    control.x0 >= card.x0
                        && control.x1 <= card.x1
                        && control.y0 >= card.y0
                        && control.y1 <= card.y1
                        && control.height() >= 32.0,
                    "large-text action {control:?} is not a contained target in {card:?}"
                );
            }
        }
        for (index, (card, ..)) in cards.iter().enumerate() {
            for (other, ..) in cards.iter().skip(index + 1) {
                assert!(
                    card.x1 <= other.x0
                        || other.x1 <= card.x0
                        || card.y1 <= other.y0
                        || other.y1 <= card.y0,
                    "large-text cards overlap: {card:?} and {other:?}"
                );
            }
        }
    }
}

#[test]
fn a_photograph_is_kept_sealed_and_a_member_opens_it() {
    let bytes = pixels::encode_png(&shots_pattern(300, 200)).expect("the pattern encodes");
    let writer = tilo_content::ContentStore::memory_in_workspace().sealing();
    let dispatch = ui::framework::testing::dispatcher("so.tilo.photos");
    let app = PhotosApp::with_content(writer.clone()).with_dispatcher(dispatch.clone());
    let mut doc = AutoCommit::new();
    PhotoLibrary::initialize(&mut doc).expect("a new library initializes");
    app.state().open_contents("harbour.png", &bytes);
    ui::framework::testing::resolve(&dispatch, &mut doc).expect("the photograph imports");

    let library = library(&doc);
    let photo = library.all_photos().next().expect("one photograph").clone();
    let reference = photo
        .image
        .content
        .reference()
        .expect("the picture is kept outside the record");
    assert!(tilo_content::is_sealed(&reference), "{reference}");
    assert_eq!(
        photo.image.hash(),
        tilo_content::hash_bytes(&bytes),
        "the record names the bytes it was opened from"
    );
    assert!(
        !writer.has(&tilo_content::hash_bytes(&bytes)),
        "the store holds no plaintext"
    );
    assert_eq!(
        photos::model::bytes_of(&photo.image, &writer),
        Some(bytes.clone()),
        "the window that opened it shows it"
    );

    let address = tilo_content::content_address(&reference).expect("a reference");
    let carried = writer.read(&address).expect("the ciphertext");
    assert_ne!(carried, bytes, "the ciphertext is not the picture");
    let member = tilo_content::ContentStore::memory_in_workspace();
    member.put_public(&carried).expect("kept as carried");
    assert_eq!(
        photos::model::bytes_of(&photo.image, &member),
        Some(bytes.clone())
    );

    let plain = tilo_content::ContentStore::memory_in_workspace();
    let hash = plain.put(&bytes).expect("kept in the clear");
    let earlier = photos::OpenImage::new(
        "harbour.png",
        300,
        200,
        ui::tilo_resource::Bytes::kept_as(&bytes, &hash),
    );
    assert_eq!(photos::model::bytes_of(&earlier, &plain), Some(bytes));
}
