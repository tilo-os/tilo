//! Editing a long note keeps the page where the person is working: pasting
//! an image or opening the slash menu below the fold used to throw the page
//! back to its top (a paste re-binds the open note, and a re-bind reset the
//! scroll offset).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::float_cmp,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::fmt::Write as _;

use crate::framework_support::{surface, with_note};
use notes::NotesShell;
use ui::clipboard::ClipboardImage;
use ui::framework::testing::TestApp;
use winit::event::{MouseScrollDelta, TouchPhase, WindowEvent};

fn long_note() -> TestApp<NotesShell> {
    let mut body = String::new();
    for i in 0..80 {
        let _ = writeln!(body, "Paragraph number {i} with some words.\n");
    }
    let (mut app, _) = with_note("Long page", &body);
    app.harness().relayout_at((1200, 700)).unwrap();
    app
}

fn scroll_y(app: &mut TestApp<NotesShell>) -> f64 {
    surface(app, |notes| notes.main_panel_mut().scroll_y_for_test())
}

/// Scroll down the page and put the caret in a block that is on screen.
fn work_below_the_fold(app: &mut TestApp<NotesShell>) -> f64 {
    for _ in 0..6 {
        app.harness().dispatch_to_root(&WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(0.0, -10.0),
            phase: TouchPhase::Moved,
        });
    }
    let rect = app.harness().targets()["block_editor"];
    app.harness()
        .click_at(rect.x0 + 200.0, f64::midpoint(rect.y0, rect.y1.min(650.0)));
    let before = scroll_y(app);
    assert!(before > 200.0, "the page is scrolled down: {before}");
    before
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 251) as u8, (y % 241) as u8, 90])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    bytes.into_inner()
}

#[test]
fn pasting_an_image_keeps_the_page_where_it_is() {
    let (services, recorder) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut app = long_note();
    let before = work_below_the_fold(&mut app);
    recorder.copy_image_elsewhere(ClipboardImage {
        mime: "image/png",
        bytes: png(120, 80),
    });
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("v");
    app.harness().set_modifiers(&[]);
    for _ in 0..3 {
        app.harness().relayout();
    }
    let after = scroll_y(&mut app);
    assert!(
        (after - before).abs() < 120.0,
        "paste jumped the page from {before} to {after}"
    );
}

#[test]
fn opening_the_slash_menu_keeps_the_page_where_it_is() {
    let mut app = long_note();
    let before = work_below_the_fold(&mut app);
    app.harness().type_text(" /");
    for _ in 0..3 {
        app.harness().relayout();
    }
    assert!(surface(&mut app, |notes| notes.editor_slash_menu_visible_for_test()));
    let after = scroll_y(&mut app);
    assert!(
        (after - before).abs() < 60.0,
        "the slash menu jumped the page from {before} to {after}"
    );
}
