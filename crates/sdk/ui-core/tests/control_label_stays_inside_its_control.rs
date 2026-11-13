//! A control never paints its label outside itself.
//!
//! This guards a defect that shipped: the page rail's add button measured its
//! label but never compared the measurement with the button's own width, so a
//! 112 px rail in German drew "Importieren …" past the button's right edge and
//! into the page gutter.
//!
//! The sweep is the invariant, at the toolkit so every app inherits it: for
//! each rail width an app can give the rail and each label an app's catalogue
//! can hand it, the label either stands inside the button or is not painted at
//! all (the button shows its glyph alone and carries the words as its name).

use tilo_ui::theme::Theme;
use tilo_ui::widget::PageSidebar;

/// Labels as apps hand the rail: the PDF reader's import affordance in both
/// languages, a slide rail's new slide, and a longer one than either.
const LABELS: [&str; 6] = [
    "Importieren …",
    "Import…",
    "Neue Folie",
    "Seite hinzufügen …",
    "New slide",
    "Open a page…",
];

/// Rail widths an app gives the rail, from a rail narrower than a control down
/// to one with room to spare.
const WIDTHS: [f64; 8] = [48.0, 64.0, 96.0, 112.0, 128.0, 160.0, 240.0, 320.0];

/// Half a pixel of slack for the shaper's rounding, as the button's own
/// elision keeps.
const SLACK: f64 = 0.5;

#[test]
fn the_add_button_never_paints_its_label_outside_itself() {
    for theme in [Theme::dark(), Theme::light()] {
        for width in WIDTHS {
            for label in LABELS {
                let mut rail = PageSidebar::new(792.0 / 612.0).with_add(label);
                let rect = vello::kurbo::Rect::new(0.0, 0.0, width, 640.0);
                rail.begin_frame_with_theme(&theme, rect, 3, 0);
                let mut scene = vello::Scene::new();
                let mut text = tilo_ui::TextRenderer::new();
                rail.render(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                    |_, _, _, _, _| {},
                );

                let button = rail.add_button_rect();
                let painted = rail.add_label_rect();
                assert!(
                    button.width() > 0.0,
                    "{label:?} on a {width} rail: the add button has a face"
                );
                if painted.is_zero_area() {
                    continue;
                }
                assert!(
                    painted.x0 >= button.x0 - SLACK
                        && painted.x1 <= button.x1 + SLACK
                        && painted.y0 >= button.y0 - SLACK
                        && painted.y1 <= button.y1 + SLACK,
                    "{label:?} on a {width} rail paints {painted:?} outside its button {button:?}"
                );
            }
        }
    }
}

/// The control keeps its words when it has the room, so the invariant above
/// does not pass by painting nothing.
#[test]
fn a_rail_with_room_for_the_words_paints_them() {
    let theme = Theme::dark();
    let mut rail = PageSidebar::new(792.0 / 612.0).with_add("Import…");
    rail.begin_frame_with_theme(
        &theme,
        vello::kurbo::Rect::new(0.0, 0.0, 320.0, 640.0),
        3,
        0,
    );
    let mut scene = vello::Scene::new();
    let mut text = tilo_ui::TextRenderer::new();
    rail.render(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
        |_, _, _, _, _| {},
    );
    assert!(
        !rail.add_label_rect().is_zero_area(),
        "a wide rail shows the add button's label"
    );

    // Below that, the German reader's rail shows the glyph alone: the words
    // live in the tooltip and the accessibility tree instead.
    let mut narrow = PageSidebar::new(792.0 / 612.0).with_add("Importieren …");
    narrow.begin_frame_with_theme(
        &theme,
        vello::kurbo::Rect::new(0.0, 0.0, 112.0, 640.0),
        3,
        0,
    );
    narrow.render(
        &mut scene,
        vello::kurbo::Affine::IDENTITY,
        &mut text,
        &theme,
        |_, _, _, _, _| {},
    );
    assert!(
        narrow.add_label_rect().is_zero_area(),
        "112 px cannot hold \"Importieren …\": the button shows its glyph alone"
    );
}
