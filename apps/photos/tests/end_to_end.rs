//! The scenario, end to end: open a photograph, adjust it, get the result out.
//!
//! These drive the real widget through the same five passes `ui::App` runs —
//! sync, `build_layout`, `compute_layout`, `apply_layout`, draw — so an assertion
//! here is an assertion about the production path and not about a helper that
//! agrees with itself.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::float_cmp,
    clippy::panic,
    clippy::unreachable,
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

/// The library `doc` holds.
fn library(doc: &AutoCommit) -> PhotoLibrary {
    PhotoLibrary::read(doc).expect("the library reads")
}

/// Make `doc` hold `library`.
fn set_library(doc: &mut AutoCommit, library: &PhotoLibrary) {
    library
        .write_fields(doc, &ROOT)
        .expect("the library writes");
}

/// The library as published.
fn published(doc: &AutoCommit) -> tilo_actions::Resource {
    ui::tilo_resource::publish::<PhotoLibrary>("photos.library", "Photos", &library(doc))
}

/// A laid-out app, ready to assert on. It is mounted in the production frame
/// host on its first frame; until then the app and its document are driven
/// directly, exactly as before any frame ran.
struct Harness {
    /// The door the app submits its edits to, resolved before each look.
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
                ui::State::new(),
            )),
            dispatch,
            mounted: None,
            theme: Theme::dark(),
            size: (w, h),
        }
    }

    /// The app and its document, the app presented the document's library
    /// as the framework presents it after every change.
    fn parts(&mut self) -> (&mut PhotosApp, &mut ui::State) {
        let (app, state) = if let Some(framed) = self.mounted.as_mut() {
            Framed::split(framed)
        } else {
            let (app, state) = self.unmounted.as_mut().expect("harness owns its app");
            (app, state)
        };
        // What the app submitted lands, as the host applies it after a pass.
        ui::framework::testing::resolve(&self.dispatch, &mut state.doc)
            .expect("submitted edits apply");
        let library = library(&state.doc);
        ui::framework::ResourceSurface::<PhotoLibrary, photos::actions::PhotosEffect>::present(
            app,
            &std::rc::Rc::new(library),
        );
        (app, state)
    }

    fn app(&mut self) -> &mut PhotosApp {
        self.parts().0
    }

    fn state(&mut self) -> &mut ui::State {
        self.parts().1
    }

    /// One full frame, exactly as `ui::App` runs it. Returns the drawn scene
    /// so a caller can prove drawing happened at all.
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

    /// The edits of the photograph the window shows.
    fn edits(&mut self) -> Edits {
        let id = self
            .app()
            .open_photo_id()
            .expect("a photograph is open")
            .to_owned();
        library(&self.state().doc)
            .photo(&id)
            .expect("the shown photograph is in the library")
            .edits
    }

    /// Give the photograph the window shows `edits`, as its edit action does.
    fn set_edits(&mut self, edits: Edits) {
        let id = self
            .app()
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

/// Write a generated pattern to a real file and return its path. Nothing in
/// the crate ships a sample image; this is a fixture, made here.
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
        let mut state = ui::State::new();
        let dispatch = ui::framework::testing::dispatcher("so.tilo.photos");
        let mut app = PhotosApp::new().with_dispatcher(dispatch.clone());
        app.open_path(&path);
        ui::framework::testing::resolve(&dispatch, &mut state.doc).expect("the photograph imports");
        app.show_library();
        let theme = Theme::high_contrast()
            .with_text_scale(2.0)
            .with_reduce_motion(true);
        let mut driver =
            ui::headless::Driver::new_with_theme(state, Box::new(app), (390, 844), theme);
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

    // The destination picker is an overlay, but its controls still have to be
    // individually routable through the same stable album ids an agent or screen
    // reader uses.  In particular, the shared modal close control must not
    // duplicate Cancel's semantic identity.
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
        hz.app().open_path(&path);
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
        app.show_selection_for_shot();
        app.show_selection_destination_chooser_for_shot();
        hz.frame();

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

// ---------------------------------------------------------------------------
// 1. An image loads and lays out inside its viewport.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn photographs_fit_the_canvas_and_non_images_open_nothing() {
    // The anti-clipping assertion, at a spread of window sizes including ones far
    // narrower and far shorter than the photograph.
    //
    // Two things are checked at each size. The chrome must tile the window
    // exactly — a toolbar, a canvas and a status line that together overflow the
    // frame is how content gets cut off with no scrollbar and no warning. And the
    // canvas must be showing the *whole* photograph: `visible_image_rect` is the
    // unit square only when every image pixel maps inside the canvas rect, so
    // anything less than the full rect means part of the picture is off-screen.
    eprintln!("phase the_whole_photograph_is_inside_the_canvas_at_every_window_size");
    {
        let dir = tempfile::tempdir().unwrap();
        // Deliberately much larger than any of the viewports below.
        let (path, _) = staged(dir.path(), 2400, 1600);

        for (w, h) in [
            (1280.0, 800.0),
            (640.0, 480.0),
            (400.0, 900.0),  // taller than wide: the photograph is not
            (1600.0, 300.0), // a letterbox strip
            (320.0, 240.0),  // small enough that chrome dominates
        ] {
            let mut hz = Harness::new(w, h);
            hz.app().open_path(&path);
            hz.frame();

            assert!(hz.app().has_image(), "{w}x{h}: the photograph did not load");

            let window = hz.window_rect();
            let canvas = hz.app().canvas_rect();
            let toolbar = hz.app().toolbar_rect();
            let status = hz.app().status_rect();

            // Chrome and canvas stay inside the window.
            for (name, r) in [("toolbar", toolbar), ("canvas", canvas), ("status", status)] {
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

            // And the photograph fits inside that canvas, entirely.
            let visible = hz.app().visible_rect();
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

    // A portrait source is the stress case for a narrow portrait window: its
    // initial view must still be contain-fit rather than inheriting an inspection
    // zoom or presenting a cropped corner as though it were the whole image.
    eprintln!("phase a_portrait_photograph_initially_fits_a_narrow_portrait_window");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 800, 1200);
        let mut hz = Harness::new(390.0, 844.0);
        hz.app().open_path(&path);
        hz.frame();

        let visible = hz.app().visible_rect();
        assert!(
            visible.is_full(),
            "a newly opened portrait photograph was clipped instead of fitted: {visible:?}"
        );
    }

    // A frame with a photograph loaded must actually put something in the scene.
    // A layout that is correct while nothing is drawn would pass every geometry
    // assertion above.
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

        // With nothing open the scene is chrome and an empty state — no image.
        let empty = hz.frame();
        assert_eq!(
            images(&empty),
            0,
            "an empty app drew an image from somewhere"
        );

        // Path count is deliberately not the metric: the empty state is an icon
        // plus two runs of text, so opening a photograph *reduces* the path count
        // while adding the one draw that matters.
        hz.app().open_path(&path);
        let loaded = hz.frame();
        assert_eq!(
            images(&loaded),
            1,
            "the photograph was not drawn into the scene"
        );
    }

    // With nothing open, the resource tree still exports the library's open
    // operation so agents, scripts, and search can enter the app; a full frame in
    // that state renders the empty view.
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
        assert!(!hz.app().has_image());
    }

    // Opening something that is not an image must fail honestly rather than
    // leaving a blank canvas that claims to hold a picture.
    eprintln!("phase opening_a_non_image_says_so_and_opens_nothing");
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"not a picture").unwrap();

        let mut hz = Harness::new(900.0, 600.0);
        hz.app().open_path(&path.to_string_lossy());
        hz.frame();

        assert!(!hz.app().has_image());
        assert!(hz.app().open_photo_id().is_none());
        assert!(library(&hz.state().doc).photos.is_empty());
    }
}

// ---------------------------------------------------------------------------
// 2. The edits change the pixels.
// ---------------------------------------------------------------------------

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fx/fy are literal fractions in 0..=1 in this test, so (dim - 1) * frac stays within 0..dim"
)]
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn rotate_crop_and_reset_change_exactly_the_exported_pixels() {
    // Rotating must move the pixels the app would export — asserted on the output
    // buffer and on a sampled corner, not on the quarter-turn counter.
    eprintln!("phase rotating_changes_the_exported_pixels");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 300, 200);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        let before = hz.app().edited_pixels().expect("rendered").clone();
        assert_eq!((before.w, before.h), (300, 200));

        let turned = hz.edits().rotated_right();
        hz.set_edits(turned);
        hz.frame();

        let after = hz.app().edited_pixels().expect("re-rendered").clone();
        assert_eq!((after.w, after.h), (200, 300), "the axes did not swap");
        assert_ne!(after.bytes, before.bytes, "the buffer is unchanged");

        // One clockwise turn sends source (sx, sy) to destination
        // (sh - 1 - sy, sx). Assert that exact mapping on a spread of pixels
        // rather than sampling by fraction: the two buffers have different aspect
        // ratios, so the same fraction lands on different pixels of a smooth
        // gradient and would compare near-equal colours that are not the same
        // pixel at all.
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

        // And the sun, which sits left-of-centre and high, must now be high on the
        // right — a coarse check that the turn went clockwise rather than the
        // other way, which the corner mapping alone would not catch if both the
        // rotation and this expectation were mirrored together.
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

    // Cropping must genuinely shrink the exported buffer and keep the region that
    // was asked for.
    eprintln!("phase cropping_changes_the_exported_pixels");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 400);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        let before = hz.app().edited_pixels().expect("rendered").clone();
        assert_eq!((before.w, before.h), (400, 400));

        // Keep the bottom-right quarter.
        let e = Edits::default().cropped_from_view(Rect01 {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();

        let after = hz.app().edited_pixels().expect("re-rendered").clone();
        assert_eq!((after.w, after.h), (200, 200), "the crop did not resize");
        assert!(
            after.bytes.len() < before.bytes.len(),
            "the cropped buffer is not smaller"
        );
        // Pixel (0,0) of the crop is pixel (200,200) of the original.
        assert_eq!(
            after.pixel(0, 0).unwrap(),
            original.pixel(200, 200).unwrap()
        );
        assert_eq!(
            after.pixel(5, 7).unwrap(),
            original.pixel(205, 207).unwrap()
        );
    }

    // Reset must return the exported pixels to exactly the original — byte for
    // byte, so a "reset" that merely re-crops to something near-full fails.
    eprintln!("phase reset_restores_the_original_pixels_exactly");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 240, 160);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
            x: 0.1,
            y: 0.1,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();
        assert_ne!(hz.app().edited_pixels().unwrap().bytes, original.bytes);

        hz.set_edits(Edits::default());
        hz.frame();
        assert_eq!(
            hz.app().edited_pixels().unwrap().bytes,
            original.bytes,
            "reset did not restore the original"
        );
    }

    // Cropping to what the viewport frames, through the real geometry: zoom in,
    // then crop, and the exported image must be the zoomed region.
    eprintln!("phase cropping_to_the_view_keeps_what_the_viewport_frames");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 400, 400);
        // A square window over a square photograph, so the fit is exact and the
        // arithmetic below is not fighting a letterbox.
        let mut hz = Harness::new(600.0, 600.0 + 48.0 + 28.8);
        hz.app().open_path(&path);
        hz.frame();

        hz.app().set_zoom(2.0);
        hz.frame();

        let view = hz.app().visible_rect();
        assert!(
            (view.w - 0.5).abs() < 0.05,
            "a 2x zoom should frame about half the width, got {view:?}"
        );

        let e = hz.edits().cropped_from_view(view);
        hz.set_edits(e);
        hz.frame();

        let after = hz.app().edited_pixels().expect("re-rendered");
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

// ---------------------------------------------------------------------------
// 3. The result gets out.
// ---------------------------------------------------------------------------

/// Records whatever is offered to the clipboard, so the copy path can be
/// asserted on real bytes instead of on its return value.
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
    // Copy must put the *edited* pixels on the clipboard — the bytes the user is
    // looking at, not the file that was opened — and must refuse when there is
    // nothing open.
    //
    eprintln!("phase copy_puts_the_edited_pixels_on_the_clipboard");
    {
        let recorder = Arc::new(RecordingClipboard::default());
        let services = ui::host::HostServices::null().with_clipboard(recorder.clone());
        let _host = ui::host::enter(&services);

        // Nothing open: the copy is refused and nothing is offered.
        {
            let mut empty = Harness::new(900.0, 600.0);
            empty.frame();
            assert!(!empty.app().copy_to_clipboard(), "copied with nothing open");
            assert!(
                recorder.image.lock().unwrap().is_none(),
                "a refused copy still offered something"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 300);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        // Turn it, then crop to the bottom-right quarter of what is shown.
        let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
            x: 0.5,
            y: 0.5,
            w: 0.5,
            h: 0.5,
        });
        hz.set_edits(e);
        hz.frame();

        let shown = hz.app().edited_pixels().expect("rendered").clone();
        assert!(hz.app().copy_to_clipboard(), "the copy was refused");

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

    // Save must write a real PNG that decodes back to exactly what was shown.
    eprintln!("phase save_writes_a_png_that_decodes_to_what_was_shown");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 320, 240);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        let turned = hz.edits().rotated_left();
        hz.set_edits(turned);
        hz.frame();
        let shown = hz.app().edited_pixels().expect("rendered").clone();

        let out = dir.path().join("out.png");
        assert!(hz.app().save_to(&out.to_string_lossy()), "save failed");

        let written = std::fs::read(&out).unwrap();
        assert_eq!(&written[1..4], b"PNG", "that is not a PNG");
        let back = pixels::decode(&written).expect("the saved file does not decode");
        assert_eq!((back.w, back.h), (shown.w, shown.h));
        assert_eq!(
            back.bytes, shown.bytes,
            "the saved pixels differ from the shown ones"
        );
    }

    // The default destination must not be the file that was opened.
    eprintln!("phase save_does_not_default_to_overwriting_the_original");
    {
        let open = photos::OpenImage::from_path("/var/tmp/tilo/home/quinten/harbour.png", 10, 10);
        let suggested = PhotosApp::suggested_save_path(&open);
        assert_ne!(suggested, open.path);
        assert!(suggested.ends_with("harbour-edited.png"), "got {suggested}");
    }
}

// ---------------------------------------------------------------------------
// 4. The resource surface.
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn resource_surface_acts_on_the_open_photograph_without_a_display() {
    // The open photograph is enumerable by id and title, its actions invoke, and
    // listing them changes nothing.
    eprintln!("phase the_resource_surface_enumerates_the_open_photograph_and_acts_on_it");
    {
        use tilo_actions::{ActionRegistry, flatten};

        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 400, 200);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        // Enumerating is a pure read.
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
            .open_photo_id()
            .expect("the photograph is open")
            .to_owned();
        let photo = resources[0]
            .children
            .iter()
            .find(|child| child.id.0 == photos::resource::photo_resource_id(&shown))
            .expect("the open photograph is published in the library");
        assert_eq!(photo.label, "harbour.png");
        // The size is legible from what the photograph reads as — no
        // display needed.
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

        // Every row is addressable by a composite key with no coordinate in it.
        for row in &flat {
            assert!(!row.key.is_empty());
            assert!(
                !row.key.contains("px") && !row.key.chars().any(|c| c == '@'),
                "an action key looks positional: {}",
                row.key
            );
        }

        // Invoke rotate-right by id, and the pixels must follow.
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

        let after = hz.app().edited_pixels().expect("re-rendered");
        assert_eq!(
            (after.w, after.h),
            (200, 400),
            "invoking the published action did not rotate the pixels"
        );
    }

    // A caller with no display crops the photograph by address, and the pixels
    // that come back are the region it asked for.
    //
    // The pieces of this were each covered — the resource surface publishes a
    // `crop` spec, `parse_crop_arg` reads a string, `Edits::cropped_from_view`
    // composes a rect — but nothing joined them to the rendered buffer. This
    // walks the whole way an out-of-process caller does: find the action by its
    // id, hand it a string, decode it through the registry, apply it, and check
    // the exported pixels against the original by identity rather than by size.
    eprintln!("phase a_caller_with_no_display_crops_by_address_and_gets_those_pixels");
    {
        use tilo_actions::ActionRegistry;

        let dir = tempfile::tempdir().unwrap();
        let (path, original) = staged(dir.path(), 400, 400);
        let mut hz = Harness::new(1000.0, 700.0);
        hz.app().open_path(&path);
        hz.frame();

        // Address the photograph by the id derived from its content — no
        // enumeration, no coordinate, nothing that needs a screen.
        let resources = [published(&hz.state().doc)];
        let id = hz
            .app()
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

        let after = hz.app().edited_pixels().expect("re-rendered").clone();
        assert_eq!(
            (after.w, after.h),
            (200, 200),
            "cropping by address did not resize the exported pixels"
        );
        // The bottom-right quarter, by identity — a crop of the right *size*
        // taken from the wrong *place* would pass a dimensions-only check.
        assert_eq!(
            after.pixel(0, 0).unwrap(),
            original.pixel(200, 200).unwrap()
        );
        assert_eq!(
            after.pixel(9, 3).unwrap(),
            original.pixel(209, 203).unwrap()
        );

        // And the document still carries only the decision, never the pixels.
        let saved = hz.state().doc.save().len();
        assert!(
            saved < 4096,
            "the document grew to {saved} bytes after a crop by address"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. Contrast, measured rather than eyeballed.
// ---------------------------------------------------------------------------

#[allow(
    clippy::cast_precision_loss,
    reason = "label.chars().count() is a handful of glyphs in a toolbar tooltip, nowhere near f32's precision limit"
)]
#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn toolbar_controls_fit_legibly_at_every_size_and_theme() {
    // Every toolbar control must be reachable at every window size the app can
    // be given.
    //
    // The strip used to be seven fixed 120 px buttons, so it was always 912 px
    // wide and simply overflowed anything narrower: at 720 px Copy was cut in
    // half and Save was entirely off-screen, with no overflow affordance to
    // reach it. `the_whole_photograph_is_inside_the_canvas_at_every_window_size`
    // passed throughout, because the canvas was fine — it was the chrome above
    // it that had escaped.
    //
    // Two frames per size on purpose: the strip refits from the width the last
    // layout measured, the same way the shared `ui::widget::Toolbar` recomputes
    // from an extent its host passes in. `ui::App` redraws continuously, so the
    // settled frame is the one anybody sees.
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
            hz.app().open_path(&path);
            hz.frame();
            hz.frame();

            let window = hz.window_rect();
            let items = hz.app().toolbar_item_rects();
            let names: Vec<&str> = items.iter().map(|(name, _)| name.as_str()).collect();
            assert!(
                names.contains(&"Library")
                    && (names.contains(&"Open") || names.contains(&"More commands")),
                "{w}x{h}: the header lost Open or Library: {names:?}"
            );
            assert!(
                names.contains(&"Save") || names.contains(&"More commands"),
                "{w}x{h}: Save is neither shown nor reachable through More: {names:?}"
            );

            for (name, r) in items {
                assert!(
                    r.width() > 0.0 && r.height() > 0.0,
                    "{w}x{h}: `{name}` was squeezed to nothing: {r:?}"
                );
                assert!(
                    r.x0 >= window.x0 - 0.5 && r.x1 <= window.x1 + 0.5,
                    "{w}x{h}: `{name}` ({:.1}..{:.1}) is outside the window (0..{w}) \
                     — it cannot be clicked",
                    r.x0,
                    r.x1
                );
            }
        }
    }

    // With nothing open there is no status line, so no band is reserved for one.
    //
    // A zero-height strip is the point: the app used to keep a 29 px plate and a
    // rule under the empty state with not one glyph in it, which reads as a
    // control that failed to draw rather than as one with nothing to say.
    eprintln!("phase an_empty_app_reserves_no_status_band");
    {
        let mut hz = Harness::new(1000.0, 700.0);
        hz.frame();

        assert!(!hz.app().has_image(), "nothing should be open");
        assert_eq!(
            hz.app().status(),
            "",
            "an app with no photograph open has nothing to report"
        );
        assert_eq!(
            hz.app().status_rect().height(),
            0.0,
            "an empty status line still reserved a band: {:?}",
            hz.app().status_rect()
        );

        // And once a photograph is open the band comes back with text in it.
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 200, 150);
        hz.app().open_path(&path);
        hz.frame();
        assert!(
            hz.app().status_rect().height() > 0.0,
            "an open photograph must get a status line"
        );
        assert!(
            hz.app().status().contains("200"),
            "got `{}`",
            hz.app().status()
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
            hz.app().open_path(&path);
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
            hz.app().show_library();
            hz.frame();
            hz.frame();

            let layouts = hz.app().library_card_action_rects();
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
        hz.app().open_path(&path);
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
        hz.app().show_library();
        hz.frame();
        hz.frame();

        let cards = hz.app().library_card_action_rects();
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

    // 200% text has a deliberately compact overflow toolbar and a two-row
    // library shelf.  Keep both the actual touch targets and their measured card
    // bounds honest: screenshot review alone cannot catch a later geometry tweak
    // that puts a scaled label back through a neighboring card action.
    eprintln!("phase large_text_library_keeps_toolbar_and_card_actions_separate");
    {
        let dir = tempfile::tempdir().unwrap();
        let (path, _) = staged(dir.path(), 1200, 800);
        let mut hz = Harness::new(1440.0, 900.0);
        hz.theme = Theme::high_contrast().with_text_scale(2.0);
        hz.app().open_path(&path);
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
        hz.app().show_library();
        hz.frame();
        hz.frame();

        let toolbar = hz.app().toolbar_item_rects();
        assert!(
            toolbar.len() >= 3,
            "large text keeps the header usable: {toolbar:?}"
        );
        for (name, rect) in toolbar {
            assert!(
                rect.x0 >= 0.0 && rect.x1 <= 1440.0 && rect.y0 >= 0.0 && rect.y1 <= 900.0,
                "large-text toolbar control `{name}` escaped the viewport: {rect:?}"
            );
        }
        let cards = hz.app().library_card_action_rects();
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
