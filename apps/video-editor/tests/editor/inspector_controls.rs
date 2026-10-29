//! Inspector and transport controls as a reader operates them: the chroma
//! key and mask appear when they have something to do and write the
//! document, and the history buttons follow the host's undo stack.

use crate::hosted;

use hosted::*;
use ui::a11y::Action;
use ui::accesskit::Role;
use video_editor::model::{Clip, MaskShape, Transform};

const MARKET: &str = "Cutaway_Market.mp4";

/// Where a control's value is stored on its clip.
type Reading = fn(&Clip) -> f64;

fn has_slider(app: &mut Editor, label: &str) -> bool {
    node(app, Role::Slider, label).is_some()
}

/// The mask's own Rotation slider: the transform has one of the same name,
/// laid out above it.
fn mask_rotation(app: &mut Editor) -> ui::accesskit::NodeId {
    nodes(app, |n| {
        n.role() == Role::Slider && n.label() == Some("Rotation")
    })
    .last()
    .expect("a mask rotation slider")
    .0
}

#[test]
fn the_key_and_mask_sliders_appear_once_switched_on_and_write_the_document() {
    let mut app = sample_editor();
    let id = clip_of(&app.state(), MARKET);
    select_clip(&mut app, MARKET);
    for label in ["Tolerance", "Softness", "Spill removal", "Width", "Feather"] {
        assert!(
            !has_slider(&mut app, label),
            "{label} waits for its feature"
        );
    }

    press(&mut app, Role::CheckBox, "Switch chroma key on");
    assert!(
        app.state().clip(id).unwrap().key.enabled,
        "the switch reaches the document"
    );
    assert!(
        node(&mut app, Role::CheckBox, "Switch chroma key off").is_some(),
        "and now says what pressing it does"
    );
    press(&mut app, Role::RadioButton, "Mask shape ellipse");
    assert_eq!(app.state().clip(id).unwrap().mask.shape, MaskShape::Ellipse);

    let reads: [(&str, Reading); 5] = [
        ("Tolerance", |c| c.key.tolerance),
        ("Softness", |c| c.key.softness),
        ("Spill removal", |c| c.key.spill),
        ("Width", |c| c.mask.w),
        ("Feather", |c| c.mask.feather),
    ];
    for (label, read) in reads {
        let before = read(app.state().clip(id).unwrap());
        act(&mut app, Role::Slider, label, &Action::Increment);
        let after = read(app.state().clip(id).unwrap());
        assert!(
            after > before,
            "{label}: {before} -> {after} in the document"
        );
    }
}

#[test]
fn switching_the_key_on_gives_it_a_tolerance_that_keys() {
    let mut project = video_editor::seed::sample_project();
    let id = clip_of(&project, MARKET);
    project.clip_mut(id).unwrap().key.tolerance = 0.0;
    let mut app = editor_with(&project);
    select_clip(&mut app, MARKET);
    press(&mut app, Role::CheckBox, "Switch chroma key on");
    let key = app.state().clip(id).unwrap().key;
    assert!(
        key.is_active(),
        "on, and actually removing something: {key:?}"
    );
}

#[test]
fn a_new_mask_lands_on_the_placed_picture_and_a_later_shape_keeps_its_size() {
    let mut project = video_editor::seed::sample_project();
    let id = clip_of(&project, MARKET);
    project.clip_mut(id).unwrap().transform = Transform::pip(0.3, (1.0, -1.0), 0.04);
    let placed = project.clip(id).unwrap().transform;
    let mut app = editor_with(&project);
    select_clip(&mut app, MARKET);

    press(&mut app, Role::RadioButton, "Mask shape ellipse");
    let mask = app.state().clip(id).unwrap().mask;
    assert!(
        (mask.x - placed.pos_x).abs() < 1e-9,
        "centred on the inset: {mask:?}"
    );
    assert!((mask.y - placed.pos_y).abs() < 1e-9, "{mask:?}");
    assert!(mask.w < 0.5 && mask.h < 0.5, "sized to the inset: {mask:?}");

    act(&mut app, Role::Slider, "Width", &Action::Increment);
    let adjusted = app.state().clip(id).unwrap().mask.w;
    press(&mut app, Role::RadioButton, "Mask shape rect");
    let mask = app.state().clip(id).unwrap().mask;
    assert_eq!(mask.shape, MaskShape::Rect);
    assert!(
        (mask.w - adjusted).abs() < 1e-9,
        "the adjusted width survives: {mask:?}"
    );
}

#[test]
fn turning_the_mask_leaves_the_picture_where_it_is() {
    let mut app = sample_editor();
    let id = clip_of(&app.state(), MARKET);
    select_clip(&mut app, MARKET);
    press(&mut app, Role::RadioButton, "Mask shape ellipse");
    let before = app.state().clip(id).unwrap().clone();
    let rotation = mask_rotation(&mut app);
    app.harness()
        .perform_a11y_action(rotation, &Action::Increment)
        .unwrap();
    let after = app.state().clip(id).unwrap().clone();
    assert!(
        (after.mask.rotation - before.mask.rotation).abs() > 1e-6,
        "the mask turned"
    );
    assert!(
        (after.transform.rotation - before.transform.rotation).abs() < 1e-9,
        "and the picture did not"
    );
}

#[test]
fn the_history_buttons_are_disabled_until_there_is_something_to_undo() {
    let mut app = editor_with(&video_editor::Project::new("Blank"));
    // Opening the project is itself an undo step; undo it so the history
    // starts empty.
    while app.undo() {}
    let (_, undo) = node(&mut app, Role::Button, "Undo").unwrap();
    assert!(undo.is_disabled(), "an empty history disables Undo");
    assert_eq!(undo.description(), Some("Nothing to undo"));
    let markers = app.state().markers.len();
    let (undo, _) = node(&mut app, Role::Button, "Undo").unwrap();
    let _ = app.harness().perform_a11y_action(undo, &Action::Activate);
    assert_eq!(
        app.state().markers.len(),
        markers,
        "a disabled Undo does nothing"
    );

    app.harness().key("m");
    app.harness().key("Enter");
    assert_eq!(app.state().markers.len(), markers + 1);
    let (_, undo) = node(&mut app, Role::Button, "Undo").unwrap();
    assert!(!undo.is_disabled());
    assert_ne!(
        undo.description(),
        Some("Nothing to undo"),
        "it says what it reverts"
    );
    let (_, redo) = node(&mut app, Role::Button, "Redo").unwrap();
    assert!(redo.is_disabled(), "redo is gated separately");

    press(&mut app, Role::Button, "Undo");
    assert_eq!(
        app.state().markers.len(),
        markers,
        "Undo reverted the marker"
    );
    // The host serves the request; the next frame presents its new history.
    app.settle();
    press(&mut app, Role::Button, "Redo");
    assert_eq!(app.state().markers.len(), markers + 1, "Redo put it back");
}
