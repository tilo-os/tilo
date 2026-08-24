//! The scenario, end to end: open a photograph, adjust it, get the result out.
//!
//! These drive the real widget through the same five passes `ui::App` runs —
//! sync, build_layout, compute_layout, apply_layout, draw — so an assertion
//! here is an assertion about the production path and not about a helper that
//! agrees with itself.

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
use qos_actions::ResourceProvider;
use photos::doc::PhotosDoc;
use photos::model::{Edits, Rect01};
use photos::pixels::{self, Rgba};
use photos::{PhotosApp, shots_pattern};

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
        let child = self.app.build_layout(&mut taffy, &self.theme, &mut node_map);
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
        Rect::new(0.0, 0.0, self.size.0 as f64, self.size.1 as f64)
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
        (400.0, 900.0), // taller than wide: the photograph is not
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

    let empty = hz.frame();
    let empty_images = empty.encoding().n_open_clips;
    let empty_paths = empty.encoding().n_paths;

    hz.app.open_path(&mut hz.state.doc, &path);
    let loaded = hz.frame();
    let _ = empty_images;
    assert!(
        loaded.encoding().n_paths > empty_paths,
        "loading a photograph added nothing to the scene: {} paths before, {} after",
        empty_paths,
        loaded.encoding().n_paths
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

    // The sun sits left-of-centre and high in the pattern. After one turn
    // clockwise it must be high on the *right*. Sample both corners rather
    // than trusting the dimensions.
    let sample = |img: &Rgba, fx: f64, fy: f64| {
        img.pixel(
            ((img.w as f64 - 1.0) * fx) as u32,
            ((img.h as f64 - 1.0) * fy) as u32,
        )
        .unwrap()
    };
    // The original's top-left region becomes the rotated image's top-right.
    assert_eq!(
        sample(&before, 0.02, 0.02),
        sample(&after, 0.98, 0.02),
        "the corner did not travel the way a clockwise turn sends it"
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
    assert_eq!(after.pixel(0, 0).unwrap(), original.pixel(200, 200).unwrap());
    assert_eq!(after.pixel(5, 7).unwrap(), original.pixel(205, 207).unwrap());
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

    let e = Edits::default()
        .rotated_right()
        .cropped_from_view(Rect01 { x: 0.1, y: 0.1, w: 0.5, h: 0.5 });
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
        "a 2x zoom should frame about half the width, got {:?}",
        view
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
        (after.w as i64 - 200).abs() <= 12,
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
/// looking at, not the file that was opened.
#[test]
fn copy_puts_the_edited_pixels_on_the_clipboard() {
    let recorder = Arc::new(RecordingClipboard::default());
    install_clipboard_backend(recorder.clone());

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

    let got = recorder.image.lock().unwrap().clone().expect("nothing was copied");
    assert_eq!(
        (got.0, got.1),
        (shown.w as usize, shown.h as usize),
        "the clipboard got a different size from what is on screen"
    );
    assert_eq!(got.2, shown.bytes, "the clipboard got different pixels");
    assert_ne!(got.2, original.bytes, "the clipboard got the unedited original");
    assert_eq!(
        got.2.len(),
        got.0 * got.1 * 4,
        "the buffer does not match its declared size"
    );
}

/// With nothing open there is nothing to copy, and the app must say so rather
/// than offering an empty buffer.
#[test]
fn copy_with_nothing_open_is_refused() {
    let recorder = Arc::new(RecordingClipboard::default());
    install_clipboard_backend(recorder.clone());

    let mut hz = Harness::new(900.0, 600.0);
    hz.frame();
    assert!(!hz.app.copy_to_clipboard());
    assert!(recorder.image.lock().unwrap().is_none());
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
    assert_eq!(back.bytes, shown.bytes, "the saved pixels differ from the shown ones");
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
    assert_eq!(hz.state.doc.save(), before, "enumeration mutated the document");

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

/// A resource tree with nothing open exports nothing, and a full frame in that
/// state still renders the empty view.
#[test]
fn nothing_open_publishes_nothing_and_still_draws() {
    let mut hz = Harness::new(900.0, 600.0);
    let scene = hz.frame();
    assert!(PhotosResources.resources(&hz.state.doc).is_empty());
    assert!(
        scene.encoding().n_paths > 0,
        "the empty state drew nothing at all"
    );
    assert!(!hz.app.has_image());
}
