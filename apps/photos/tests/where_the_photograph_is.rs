//! Where the open photograph is drawn, as the app answers it.
//!
//! `actions.rs` proves the photograph can be *addressed*:
//! `photos.image:/…/harbour.png#crop` names one operation on one object, and
//! carries no screen position — which is what lets a caller with no display
//! invoke it, and what keeps the address meaning the same thing across
//! layouts, themes and machines.
//!
//! This proves the other half, the one an address deliberately cannot carry:
//! *and whereabouts is it, right now.* Something outside this process — the
//! compositor, marking what a participant is acting on — has to turn that
//! address into pixels, and the only thing that knows is the view.
//!
//! The claim worth holding is not that a rect comes back. It is **which**
//! rect. A photograph is fitted inside its canvas, so unless it happens to
//! match the window's aspect ratio there are letterbox bands either side of
//! it, and those bands are not the photograph. A mark drawn around the canvas
//! would claim a participant had touched a region of empty background — which
//! is a picture of the wrong thing, told confidently.

use std::collections::HashMap;

use photos::PhotosApp;
use photos::doc::PhotosDoc;
use taffy::prelude::*;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;

const VIEWPORT: (f32, f32) = (1280.0, 800.0);

/// An app showing a photograph of `w` x `h`, decoded from a real file through
/// the same route the chooser and the command line take.
fn opened_sized(w: u32, h: u32) -> (ui::State, PhotosApp, String, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("a staging directory");
    let path = dir.path().join("harbour.png");
    let png = photos::pixels::encode_png(&photos::shots_pattern(w, h)).expect("encodes");
    std::fs::write(&path, png).expect("writes");
    let path = path.to_string_lossy().into_owned();

    let mut state = ui::State::new();
    let mut app = PhotosApp::new();
    PhotosDoc::seed(&mut state.doc);
    app.open_path(&mut state.doc, &path);
    assert!(
        PhotosDoc::image(&state.doc).is_some(),
        "the fixture failed to open a photograph, so nothing below proves anything"
    );
    (state, app, path, dir)
}

/// Lay the app out at the viewport size, the way `ui::App` does every frame.
///
/// Not decoration: the viewer's framing is only real once a layout has
/// happened, and where the photograph ended up is a fact about that layout.
fn layout(app: &mut PhotosApp, state: &mut ui::State) {
    let (w, h) = VIEWPORT;
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    app.sync(&mut state.doc, &state.root);
    let child = app.build_layout(&mut taffy, &theme, &mut node_map);
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
        let trr = &mut tr;
        taffy
            .compute_layout_with_measure(
                root,
                taffy::Size {
                    width: AvailableSpace::Definite(w),
                    height: AvailableSpace::Definite(h),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = measure_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, trr) }
                    } else {
                        taffy::Size::ZERO
                    }
                },
            )
            .unwrap();
    }
    app.apply_layout(&taffy, child, Point::ORIGIN);
}

/// An empty viewer publishes no place, because it is showing nothing.
///
/// The honest answer, and the one that keeps the mark truthful: an address
/// that resolves to no rect draws nothing at all, so a window with no
/// photograph in it can never be marked as though it had one.
#[test]
fn a_viewer_with_nothing_open_publishes_no_place() {
    let mut state = ui::State::new();
    let mut app = PhotosApp::new();
    PhotosDoc::seed(&mut state.doc);
    layout(&mut app, &mut state);
    assert!(
        Widget::resource_rects(&app).is_empty(),
        "an empty viewer named a place for a photograph it does not have"
    );
}

/// The address a caller invokes is the address that resolves to a place.
///
/// Two halves minted from the same path by the same function. If they ever
/// drift, every invocation still works and nothing is ever marked — the
/// failure that looks exactly like a desktop where the agent did not act.
#[test]
fn the_place_is_keyed_by_the_address_a_caller_invokes() {
    let (mut state, mut app, path, _dir) = opened_sized(900, 600);
    layout(&mut app, &mut state);

    let rects = Widget::resource_rects(&app);
    assert_eq!(rects.len(), 1, "one open photograph, one place: {rects:?}");
    let id = photos::actions::resource_id(&path);
    assert_eq!(rects[0].0, id, "the place is not keyed by the address");

    let published = Widget::resources(&app, &state.doc);
    assert!(
        published.iter().any(|r| r.id == id),
        "the resource tree publishes no {id}, so the address that resolves to \
         a place is not one anybody can invoke"
    );
}

/// The place is the **photograph**, not the canvas it is fitted into.
///
/// A tall picture in a wide window leaves real letterbox bands. The rect that
/// comes back has to exclude them: a mark drawn around the canvas would put
/// its brackets on empty background and claim that is what was touched.
#[test]
fn the_place_is_the_picture_and_not_the_bands_beside_it() {
    let (mut state, mut app, _path, _dir) = opened_sized(600, 1000);
    layout(&mut app, &mut state);

    let rects = Widget::resource_rects(&app);
    assert_eq!(rects.len(), 1, "expected one photograph: {rects:?}");
    let picture = rects[0].1;
    let window = Widget::get_rect(&app);

    assert!(
        picture.width() > 1.0 && picture.height() > 1.0,
        "a photograph with no area cannot be pointed at: {picture:?}"
    );
    assert!(
        picture.width() < window.width() - 2.0,
        "a picture taller than its window must leave bands either side, and \
         the published place must not include them — picture {picture:?} in \
         window {window:?}"
    );
    assert!(
        window.contains(picture.origin())
            && picture.x1 <= window.x1 + 0.5
            && picture.y1 <= window.y1 + 0.5,
        "the published place runs outside the window it is in — picture \
         {picture:?} in window {window:?}"
    );
}

/// Editing the photograph moves the place it is drawn in.
///
/// This is why nothing may cache a rect taken at the moment an action was
/// invoked: cropping to a different shape re-fits the picture, so the very
/// operation being marked changes where the mark belongs. Frozen coordinates
/// would leave the brackets beside the thing they are about.
#[test]
fn cropping_moves_the_place_the_mark_belongs_on() {
    let (mut state, mut app, _path, _dir) = opened_sized(1200, 400);
    layout(&mut app, &mut state);
    let before = Widget::resource_rects(&app)[0].1;

    // Through the app's own action, as a caller invoking `#crop` reaches it.
    let action = photos::actions::PhotosAction::SetEdits {
        crop: photos::model::Rect01 {
            x: 0.30,
            y: 0.05,
            w: 0.40,
            h: 0.90,
        },
        rotate_quarters: 0,
    };
    qos_actions::Action::apply(&action, &mut state.doc).expect("the crop applies");
    layout(&mut app, &mut state);
    let after = Widget::resource_rects(&app)[0].1;

    assert_ne!(
        (before.x0, before.y0, before.x1, before.y1),
        (after.x0, after.y0, after.x1, after.y1),
        "a wide photograph cropped to a tall one is drawn in the same place, \
         so a mark placed before the crop would still look right afterwards \
         by accident"
    );
    assert!(
        after.width() < before.width() - 1.0,
        "cropping a 1200x400 picture to two fifths of its width left it as \
         wide on screen: before {before:?}, after {after:?}"
    );
}
