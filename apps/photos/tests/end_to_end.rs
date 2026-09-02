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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use taffy::prelude::*;
use ui::clipboard::{ClipboardBackend, ClipboardTarget, install_clipboard_backend};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use photos::actions::{PhotosResources, register};
use photos::doc::PhotosDoc;
use photos::model::{Edits, Rect01};
use photos::pixels::{self, Rgba};
use photos::{PhotosApp, shots_pattern};
use qos_actions::ResourceProvider;

/// A laid-out app, ready to assert on.
struct Harness {
    app: PhotosApp,
    state: ui::State,
    theme: Theme,
    text: TextRenderer,
    size: (f32, f32),
}

impl Harness {
    fn new(w: f32, h: f32) -> Self {
        Self {
            app: PhotosApp::new(),
            state: ui::State::new(),
            theme: Theme::dark(),
            text: TextRenderer::new(),
            size: (w, h),
        }
    }

    /// One full frame, exactly as `ui::App` runs it. Returns the drawn scene
    /// so a caller can prove drawing happened at all.
    fn frame(&mut self) -> Scene {
        let (w, h) = self.size;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();

        self.app.sync(&mut self.state.doc, &self.state.root);
        let child = self
            .app
            .build_layout(&mut taffy, &self.theme, &mut node_map);
        let root = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::length(w),
                        height: Dimension::length(h),
                    },
                    ..Default::default()
                },
                &[child],
            )
            .unwrap();
        {
            let measure_map = node_map.clone();
            let tr = &mut self.text;
            taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w),
                        height: AvailableSpace::Definite(h),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .unwrap();
        }
        self.app.apply_layout(&taffy, child, Point::ORIGIN);

        let mut scene = Scene::new();
        self.app.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &PresenceCtx::empty(),
        );
        scene
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

// ---------------------------------------------------------------------------
// 1. An image loads and lays out inside its viewport.
// ---------------------------------------------------------------------------

/// The anti-clipping assertion, at a spread of window sizes including ones far
/// narrower and far shorter than the photograph.
///
/// Two things are checked at each size. The chrome must tile the window
/// exactly — a toolbar, a canvas and a status line that together overflow the
/// frame is how content gets cut off with no scrollbar and no warning. And the
/// canvas must be showing the *whole* photograph: `visible_image_rect` is the
/// unit square only when every image pixel maps inside the canvas rect, so
/// anything less than the full rect means part of the picture is off-screen.
#[test]
fn the_whole_photograph_is_inside_the_canvas_at_every_window_size() {
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
        hz.app.open_path(&mut hz.state.doc, &path);
        hz.frame();

        assert!(hz.app.has_image(), "{w}x{h}: the photograph did not load");

        let window = hz.window_rect();
        let canvas = hz.app.canvas_rect();
        let toolbar = hz.app.toolbar_rect();
        let status = hz.app.status_rect();

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
        let visible = hz.app.visible_rect();
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

/// A frame with a photograph loaded must actually put something in the scene.
/// A layout that is correct while nothing is drawn would pass every geometry
/// assertion above.
#[test]
fn a_loaded_photograph_produces_a_non_empty_scene() {
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
    hz.app.open_path(&mut hz.state.doc, &path);
    let loaded = hz.frame();
    assert_eq!(
        images(&loaded),
        1,
        "the photograph was not drawn into the scene"
    );
}

/// Opening something that is not an image must fail honestly rather than
/// leaving a blank canvas that claims to hold a picture.
#[test]
fn opening_a_non_image_says_so_and_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes.txt");
    std::fs::write(&path, b"not a picture").unwrap();

    let mut hz = Harness::new(900.0, 600.0);
    hz.app.open_path(&mut hz.state.doc, &path.to_string_lossy());
    hz.frame();

    assert!(!hz.app.has_image());
    assert!(PhotosDoc::image(&hz.state.doc).is_none());
}

// ---------------------------------------------------------------------------
// 2. The edits change the pixels.
// ---------------------------------------------------------------------------

/// Rotating must move the pixels the app would export — asserted on the output
/// buffer and on a sampled corner, not on the quarter-turn counter.
#[test]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "fx/fy are literal fractions in 0..=1 in this test, so (dim - 1) * frac stays within 0..dim"
)]
fn rotating_changes_the_exported_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = staged(dir.path(), 300, 200);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    let before = hz.app.edited_pixels().expect("rendered").clone();
    assert_eq!((before.w, before.h), (300, 200));

    let turned = PhotosDoc::edits(&hz.state.doc).rotated_right();
    PhotosDoc::set_edits(&mut hz.state.doc, &turned);
    hz.frame();

    let after = hz.app.edited_pixels().expect("re-rendered").clone();
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

/// Cropping must genuinely shrink the exported buffer and keep the region that
/// was asked for.
#[test]
fn cropping_changes_the_exported_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = staged(dir.path(), 400, 400);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    let before = hz.app.edited_pixels().expect("rendered").clone();
    assert_eq!((before.w, before.h), (400, 400));

    // Keep the bottom-right quarter.
    let e = Edits::default().cropped_from_view(Rect01 {
        x: 0.5,
        y: 0.5,
        w: 0.5,
        h: 0.5,
    });
    PhotosDoc::set_edits(&mut hz.state.doc, &e);
    hz.frame();

    let after = hz.app.edited_pixels().expect("re-rendered").clone();
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

/// Reset must return the exported pixels to exactly the original — byte for
/// byte, so a "reset" that merely re-crops to something near-full fails.
#[test]
fn reset_restores_the_original_pixels_exactly() {
    let dir = tempfile::tempdir().unwrap();
    let (path, original) = staged(dir.path(), 240, 160);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
        x: 0.1,
        y: 0.1,
        w: 0.5,
        h: 0.5,
    });
    PhotosDoc::set_edits(&mut hz.state.doc, &e);
    hz.frame();
    assert_ne!(hz.app.edited_pixels().unwrap().bytes, original.bytes);

    PhotosDoc::set_edits(&mut hz.state.doc, &Edits::default());
    hz.frame();
    assert_eq!(
        hz.app.edited_pixels().unwrap().bytes,
        original.bytes,
        "reset did not restore the original"
    );
}

/// Cropping to what the viewport frames, through the real geometry: zoom in,
/// then crop, and the exported image must be the zoomed region.
#[test]
fn cropping_to_the_view_keeps_what_the_viewport_frames() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = staged(dir.path(), 400, 400);
    // A square window over a square photograph, so the fit is exact and the
    // arithmetic below is not fighting a letterbox.
    let mut hz = Harness::new(600.0, 600.0 + 48.0 + 28.8);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    hz.app.set_zoom(2.0);
    hz.frame();

    let view = hz.app.visible_rect();
    assert!(
        (view.w - 0.5).abs() < 0.05,
        "a 2x zoom should frame about half the width, got {view:?}"
    );

    let e = PhotosDoc::edits(&hz.state.doc).cropped_from_view(view);
    PhotosDoc::set_edits(&mut hz.state.doc, &e);
    hz.frame();

    let after = hz.app.edited_pixels().expect("re-rendered");
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

/// Copy must put the *edited* pixels on the clipboard — the bytes the user is
/// looking at, not the file that was opened — and must refuse when there is
/// nothing open.
///
/// Both halves live in one test deliberately: the clipboard backend is
/// process-global and the harness runs tests in parallel threads, so two tests
/// each installing their own recorder would race over which one the copy path
/// resolves to.
#[test]
fn copy_puts_the_edited_pixels_on_the_clipboard() {
    let recorder = Arc::new(RecordingClipboard::default());
    install_clipboard_backend(recorder.clone());

    // Nothing open: the copy is refused and nothing is offered.
    {
        let mut empty = Harness::new(900.0, 600.0);
        empty.frame();
        assert!(!empty.app.copy_to_clipboard(), "copied with nothing open");
        assert!(
            recorder.image.lock().unwrap().is_none(),
            "a refused copy still offered something"
        );
    }

    let dir = tempfile::tempdir().unwrap();
    let (path, original) = staged(dir.path(), 400, 300);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    // Turn it, then crop to the bottom-right quarter of what is shown.
    let e = Edits::default().rotated_right().cropped_from_view(Rect01 {
        x: 0.5,
        y: 0.5,
        w: 0.5,
        h: 0.5,
    });
    PhotosDoc::set_edits(&mut hz.state.doc, &e);
    hz.frame();

    let shown = hz.app.edited_pixels().expect("rendered").clone();
    assert!(hz.app.copy_to_clipboard(), "the copy was refused");

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

/// Save must write a real PNG that decodes back to exactly what was shown.
#[test]
fn save_writes_a_png_that_decodes_to_what_was_shown() {
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = staged(dir.path(), 320, 240);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    let turned = PhotosDoc::edits(&hz.state.doc).rotated_left();
    PhotosDoc::set_edits(&mut hz.state.doc, &turned);
    hz.frame();
    let shown = hz.app.edited_pixels().expect("rendered").clone();

    let out = dir.path().join("out.png");
    assert!(hz.app.save_to(&out.to_string_lossy()), "save failed");

    let written = std::fs::read(&out).unwrap();
    assert_eq!(&written[1..4], b"PNG", "that is not a PNG");
    let back = pixels::decode(&written).expect("the saved file does not decode");
    assert_eq!((back.w, back.h), (shown.w, shown.h));
    assert_eq!(
        back.bytes, shown.bytes,
        "the saved pixels differ from the shown ones"
    );
}

/// The default destination must not be the file that was opened.
#[test]
fn save_does_not_default_to_overwriting_the_original() {
    let open = photos::OpenImage::from_path("/var/tmp/qos/home/quinten/harbour.png", 10, 10);
    let suggested = PhotosApp::suggested_save_path(&open);
    assert_ne!(suggested, open.path);
    assert!(suggested.ends_with("harbour-edited.png"), "got {suggested}");
}

// ---------------------------------------------------------------------------
// 4. The resource surface.
// ---------------------------------------------------------------------------

/// The open photograph is enumerable by id and title, its actions invoke, and
/// listing them changes nothing.
#[test]
fn the_resource_surface_enumerates_the_open_photograph_and_acts_on_it() {
    use qos_actions::{ActionRegistry, flatten};

    let dir = tempfile::tempdir().unwrap();
    let (path, _) = staged(dir.path(), 400, 200);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    // Enumerating is a pure read.
    let before = hz.state.doc.save();
    let resources = PhotosResources.resources(&hz.state.doc);
    let flat = flatten(&resources);
    assert_eq!(
        hz.state.doc.save(),
        before,
        "enumeration mutated the document"
    );

    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].label, "harbour.png");
    // The size is legible from titles alone — no display needed.
    assert_eq!(resources[0].children[0].label, "400 x 200 pixels");

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
    register(&mut registry);
    let spec = resources[0]
        .actions
        .iter()
        .find(|a| a.id == "rotate-right")
        .expect("rotate-right is published");
    let qos_actions::ActionInput::Immediate(encoded) = &spec.input else {
        panic!("rotate-right fires immediately");
    };
    registry
        .decode(encoded)
        .unwrap()
        .apply(&mut hz.state.doc)
        .unwrap();
    hz.frame();

    let after = hz.app.edited_pixels().expect("re-rendered");
    assert_eq!(
        (after.w, after.h),
        (200, 400),
        "invoking the published action did not rotate the pixels"
    );
}

/// A caller with no display crops the photograph by address, and the pixels
/// that come back are the region it asked for.
///
/// The pieces of this were each covered — the resource surface publishes a
/// `crop` spec, `parse_crop_arg` reads a string, `Edits::cropped_from_view`
/// composes a rect — but nothing joined them to the rendered buffer. This
/// walks the whole way an out-of-process caller does: find the action by its
/// id, hand it a string, decode it through the registry, apply it, and check
/// the exported pixels against the original by identity rather than by size.
#[test]
fn a_caller_with_no_display_crops_by_address_and_gets_those_pixels() {
    use qos_actions::{ActionInput, ActionRegistry};

    let dir = tempfile::tempdir().unwrap();
    let (path, original) = staged(dir.path(), 400, 400);
    let mut hz = Harness::new(1000.0, 700.0);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();

    // Address the photograph by the id derived from its content — no
    // enumeration, no coordinate, nothing that needs a screen.
    let resources = PhotosResources.resources(&hz.state.doc);
    let open = photos::doc::PhotosDoc::image(&hz.state.doc).expect("the harness opened one");
    assert_eq!(
        resources[0].id.0,
        photos::actions::resource_id(&open),
        "the published id is not the one derived from the photograph"
    );

    let ActionInput::Prompted { build, .. } = &resources[0]
        .actions
        .iter()
        .find(|a| a.id == "crop")
        .expect("crop is published")
        .input
    else {
        panic!("crop collects an argument");
    };

    let mut registry = ActionRegistry::new();
    register(&mut registry);
    registry
        .decode(&build("0.5,0.5,0.5,0.5"))
        .expect("the envelope decodes")
        .apply(&mut hz.state.doc)
        .expect("the crop applies");
    hz.frame();

    let after = hz.app.edited_pixels().expect("re-rendered").clone();
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
    let saved = hz.state.doc.save().len();
    assert!(
        saved < 4096,
        "the document grew to {saved} bytes after a crop by address"
    );
}

/// A resource tree with nothing open exports only the openable library, and a
/// full frame in that state still renders the empty view.
#[test]
fn nothing_open_publishes_the_library_and_still_draws() {
    let mut hz = Harness::new(900.0, 600.0);
    let scene = hz.frame();
    let resources = PhotosResources.resources(&hz.state.doc);
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].kind, "photos.library");
    assert_eq!(resources[0].actions[0].id, "open");
    assert!(
        scene.encoding().n_paths > 0,
        "the empty state drew nothing at all"
    );
    assert!(!hz.app.has_image());
}

// ---------------------------------------------------------------------------
// 5. Contrast, measured rather than eyeballed.
// ---------------------------------------------------------------------------

/// Every colour this app chooses for itself, measured in every theme it can be
/// rendered under.
///
/// Only the pairs *this crate* picks are checked: the status line's text, and
/// the boundary that separates the toolbar and the status bar from the canvas.
/// The buttons and the empty state draw themselves from the toolkit and are
/// the toolkit's contract, not this app's.
///
/// Measured as of writing:
///
/// ```text
///                  text/base   text/raised   raised/base   border/base
///   Dark              15.30        13.89          1.10          1.29
///   Light             15.35        17.05          1.11          1.21
///   High Contrast     21.00        21.00          1.00         21.00
/// ```
///
/// Two different floors, because they are two different requirements.
///
/// Text carries the 4.5:1 body floor and clears it by a wide margin in all
/// three palettes.
///
/// The chrome boundary does *not* carry the 3.0:1 non-text floor: that floor
/// governs controls and meaningful state, not a decorative division between a
/// toolbar and the content under it, and the house palette deliberately draws
/// such divisions as hairlines — 1.29:1 in dark, 1.21:1 in light. Holding this
/// app to 3.0 there would put it out of step with every other app in the fleet
/// while fixing nothing a user could perceive as a defect.
///
/// What is asserted instead is that the boundary *exists*: whichever of the
/// two carriers is doing the work in a given theme must be non-degenerate.
/// That is the real bug this test was written after finding — under
/// high-contrast, `surfaces.raised` and `surfaces.base` are both pure black,
/// so a toolbar defined only by its plate had a separation of exactly 1.00:1
/// and no edge whatsoever. The rule the app now draws carries it there at
/// 21.00:1. A regression that removed the rule puts high-contrast back to 1.00
/// and fails this.
#[test]
fn the_colours_this_app_chooses_are_legible_in_every_theme() {
    use ui::a11y::contrast_ratio;

    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let surface = theme.tokens.colors.surfaces.base;
        let raised = theme.tokens.colors.surfaces.raised;

        // Body text against both surfaces it can sit on: 4.5:1.
        for (what, against) in [("the canvas", surface), ("the raised plate", raised)] {
            let ratio = contrast_ratio(theme.text_primary, against);
            assert!(
                ratio >= 4.5,
                "{}: the status line reads at {ratio:.2}:1 on {what}, below the 4.5 body floor",
                theme.name
            );
        }

        // The toolbar/canvas boundary must be carried by something.
        let plate = contrast_ratio(raised, surface);
        let rule = contrast_ratio(theme.border_standard, surface);
        assert!(
            plate.max(rule) >= 1.15,
            "{}: nothing separates the toolbar from the canvas (plate {plate:.2}:1, rule {rule:.2}:1)",
            theme.name
        );
    }
}

/// Every toolbar control must be reachable at every window size the app can
/// be given.
///
/// The strip used to be seven fixed 120 px buttons, so it was always 912 px
/// wide and simply overflowed anything narrower: at 720 px Copy was cut in
/// half and Save was entirely off-screen, with no overflow affordance to
/// reach it. `the_whole_photograph_is_inside_the_canvas_at_every_window_size`
/// passed throughout, because the canvas was fine — it was the chrome above
/// it that had escaped.
///
/// Two frames per size on purpose: the strip refits from the width the last
/// layout measured, the same way the shared `ui::widget::Toolbar` recomputes
/// from an extent its host passes in. `ui::App` redraws continuously, so the
/// settled frame is the one anybody sees.
#[test]
fn every_toolbar_control_is_inside_the_window_at_every_size() {
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
        hz.app.open_path(&mut hz.state.doc, &path);
        hz.frame();
        hz.frame();

        let window = hz.window_rect();
        let items = hz.app.toolbar_item_rects();
        assert_eq!(
            items.len(),
            7,
            "{w}x{h}: the toolbar lost a control entirely"
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

/// The per-character estimate `build_layout` sizes buttons with must be
/// generous enough for the glyphs `draw` actually lays down.
///
/// `build_layout` gets a `Theme` but no `TextRenderer`, so it approximates a
/// label's width at 7 px a character — the same constant the shared
/// `ui::widget::Toolbar` uses. This measures the real shaped text against
/// that estimate, so a label that outgrows it fails here rather than
/// silently rendering clipped or overlapping its neighbour.
#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "label.chars().count() is a handful of glyphs in a toolbar tooltip, nowhere near f32's precision limit"
)]
fn toolbar_labels_fit_inside_the_buttons_they_are_sized_for() {
    // Mirrors `Button::paint`: 14 px, Medium, an icon derived from the
    // button height, and a 6 px gap between the two.
    const TEXT_PX: f32 = 14.0;
    const BTN_H: f32 = 34.0;
    const ICON_W: f32 = BTN_H - 14.0;
    const ICON_GAP: f32 = 6.0;
    const LABEL_PAD_X: f32 = 12.0;

    photos::strings::install();
    let mut text = TextRenderer::new();

    for key in ["photos-open", "photos-copy", "photos-save"] {
        let label = ui::i18n::I18nManager::lookup(key, None);
        let (measured, _) =
            text.measure_weighted(&label, TEXT_PX, None, ui::text_render::TextWeight::Medium);
        let estimated = label.chars().count() as f32 * photos::app::APPROX_CHAR_W;
        let budget = LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + estimated;
        let needed = LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + measured;
        println!(
            "{label}: {} chars, shaped {measured:.1} px ({:.2} px/char), budget {estimated:.1} px",
            label.chars().count(),
            measured / label.chars().count() as f32,
        );
        assert!(
            needed <= budget,
            "`{label}` shapes to {measured:.1} px but is budgeted {estimated:.1} px \
             ({needed:.1} px needed in a {budget:.1} px button)"
        );
    }
}

/// With nothing open there is no status line, so no band is reserved for one.
///
/// A zero-height strip is the point: the app used to keep a 29 px plate and a
/// rule under the empty state with not one glyph in it, which reads as a
/// control that failed to draw rather than as one with nothing to say.
#[test]
fn an_empty_app_reserves_no_status_band() {
    let mut hz = Harness::new(1000.0, 700.0);
    hz.frame();

    assert!(!hz.app.has_image(), "nothing should be open");
    assert_eq!(
        hz.app.status(),
        "",
        "an app with no photograph open has nothing to report"
    );
    assert_eq!(
        hz.app.status_rect().height(),
        0.0,
        "an empty status line still reserved a band: {:?}",
        hz.app.status_rect()
    );

    // And once a photograph is open the band comes back with text in it.
    let dir = tempfile::tempdir().unwrap();
    let (path, _) = staged(dir.path(), 200, 150);
    hz.app.open_path(&mut hz.state.doc, &path);
    hz.frame();
    assert!(
        hz.app.status_rect().height() > 0.0,
        "an open photograph must get a status line"
    );
    assert!(hz.app.status().contains("200"), "got `{}`", hz.app.status());
}
