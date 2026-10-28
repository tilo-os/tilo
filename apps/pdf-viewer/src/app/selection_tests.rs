//! Text selection driven the way a person drives it: pointer presses,
//! drags, multi-clicks, Shift, keys, the context menu and a finger, through
//! the window's own event path over a real PDF.

use std::time::Duration;

use serial_test::serial;
use ui::headless::{Framed, viewport};
use ui::theme::Theme;
use vello::kurbo::Point;
use winit::event::{ElementState, MouseButton, TouchPhase, WindowEvent};

use super::PdfApp;
use super::tests::open_settled;

mod in_the_desktop;

const LINES: usize = 40;

/// The words of line `line` on page `page` of [`text_pdf`].
fn line_text(page: usize, line: usize) -> String {
    format!("Page {page} line {line}: the quick brown fox jumps over the lazy dog")
}

/// A PDF of `pages` pages of plain text lines, in reading order.
pub(super) fn text_pdf(dir: &std::path::Path, pages: usize) -> std::path::PathBuf {
    use std::fmt::Write as _;
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..pages).map(|i| format!("{} 0 R", 4 + i * 2)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {pages} >>",
        kids.join(" ")
    ));
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
    for p in 1..=pages {
        let mut content = String::from("0 0 0 rg BT /F1 12 Tf 18 TL 56 740 Td\n");
        for line in 0..LINES {
            let _ = writeln!(content, "({}) '", line_text(p, line));
        }
        content.push_str("ET\n");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + (p - 1) * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}endstream",
            content.len()
        ));
    }
    let mut out = String::from("%PDF-1.4\n");
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{object}\nendobj\n", i + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    let path = dir.join("text.pdf");
    std::fs::write(&path, out).expect("write the text fixture");
    path
}

/// The reader on a `pages`-page text PDF in a window, every visible page
/// drawn.
fn reader(pages: usize, theme: Theme) -> Option<(tempfile::TempDir, Framed<PdfApp>)> {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = text_pdf(dir.path(), pages);
    let app = open_settled(&path)?;
    let mut app = Framed::new(
        crate::scenarios::record_state(),
        app,
        viewport((1000, 800)),
        theme,
    );
    settle(&mut app, |app| !app.pending_visuals);
    Some((dir, app))
}

/// Run the window's frames until `done` holds, as they run while the PDF
/// worker answers.
fn settle(app: &mut Framed<PdfApp>, mut done: impl FnMut(&PdfApp) -> bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        Framed::harness_mut(app).relayout();
        if done(app) {
            return;
        }
        assert!(
            ui::time::now() < deadline,
            "the reader never settled ({:?})",
            app.status_note
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Rest the pointer on displayed page `page` until its text is here, as a
/// pointer resting on a page reads it ahead of the press.
fn hover_until_read(app: &mut Framed<PdfApp>, page: usize) {
    let origin = app.page_origins()[page - 1];
    let canvas = app.canvas_rect;
    let at = Point::new(
        (origin.x + 40.0).max(canvas.x0 + 4.0),
        (origin.y + 40.0).clamp(canvas.y0 + 4.0, canvas.y1 - 4.0),
    );
    move_to(app, at);
    settle(app, |app| app.raster_text.contains_key(&page));
}

fn move_to(app: &mut Framed<PdfApp>, at: Point) {
    Framed::harness_mut(app).send(&WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(at.x, at.y),
    });
}

/// A screen point over the leading half of glyph `nth` of the first
/// `needle` on displayed page `page`, whose text is here: a caret before it.
fn glyph_at(app: &PdfApp, page: usize, needle: &str, nth: usize) -> Point {
    glyph_point(app, page, needle, nth, 0.3)
}

/// A screen point over the trailing half of that glyph: a caret after it.
fn past(app: &PdfApp, page: usize, needle: &str, nth: usize) -> Point {
    glyph_point(app, page, needle, nth, 0.7)
}

fn glyph_point(app: &PdfApp, page: usize, needle: &str, nth: usize, across: f64) -> Point {
    let layer = &app.raster_text[&page];
    let byte = layer
        .text
        .find(needle)
        .unwrap_or_else(|| panic!("{needle:?} is on page {page}"));
    let index = layer.text[..byte].chars().count() + nth;
    let band = layer.bands_for_range(index, index + 1)[0];
    let origin = app.page_origins()[page - 1];
    Point::new(
        origin.x + (band.x + band.w * across) * app.zoom(),
        origin.y + (band.y + band.h / 2.0) * app.zoom(),
    )
}

fn drag(app: &mut Framed<PdfApp>, from: Point, to: Point) {
    Framed::harness_mut(app).drag_at((from.x, from.y), (to.x, to.y));
}

fn selected(app: &PdfApp) -> String {
    app.selection_text().unwrap_or_default()
}

#[serial(pdfium)]
#[test]
fn dragging_selects_across_lines_and_pages_in_reading_order() {
    let Some((_dir, mut app)) = reader(3, Theme::dark()) else {
        return;
    };
    eprintln!("phase a_drag_across_lines_selects_the_lines_between");
    hover_until_read(&mut app, 1);
    let from = glyph_at(&app, 1, "Page 1 line 2:", 7);
    let to = past(&app, 1, "Page 1 line 4:", 12);
    drag(&mut app, from, to);
    settle(&mut app, |app| {
        app.selection.is_some_and(|sel| !sel.dragging)
    });
    let text = selected(&app);
    assert!(
        text.starts_with("line 2:") && text.ends_with("Page 1 line 4"),
        "a drag from line 2 to line 4 selects what lies between, in order: {text:?}"
    );
    assert!(text.contains(&line_text(1, 3)), "{text:?}");
    assert!(
        app.selection_screen_bands().len() >= 3,
        "each line the selection covers is painted"
    );

    eprintln!("phase a_drag_past_the_page_end_continues_on_the_next_page");
    // The end of page 1 and the top of page 2 on screen together.
    let unit = app.stack_unit() * app.zoom();
    app.viewport.set_scroll(0.0, unit - 300.0);
    settle(&mut app, |app| !app.pending_visuals);
    hover_until_read(&mut app, 1);
    hover_until_read(&mut app, 2);
    let from = glyph_at(&app, 1, &format!("Page 1 line {}:", LINES - 2), 0);
    let to = past(&app, 2, "Page 2 line 1:", 12);
    drag(&mut app, from, to);
    settle(&mut app, |app| {
        app.selection.is_some_and(|sel| !sel.dragging)
    });
    let sel = app.selection.expect("the drag selected text");
    assert_eq!((sel.start().page, sel.end().page), (1, 2));
    let text = selected(&app);
    assert!(
        text.starts_with(&line_text(1, LINES - 2))
            && text.contains(&line_text(1, LINES - 1))
            && text.contains(&line_text(2, 0))
            && text.ends_with("Page 2 line 1"),
        "a drag from page 1 onto page 2 selects across the page break: {text:?}"
    );
    let bands = app.selection_screen_bands();
    let page_two_top = app.page_origins()[1].y;
    assert!(
        bands.iter().any(|band| band.y1 < page_two_top)
            && bands.iter().any(|band| band.y0 > page_two_top),
        "both pages paint their part of the selection"
    );
}

#[serial(pdfium)]
#[test]
fn a_double_click_selects_a_word_and_a_triple_click_its_line() {
    let Some((_dir, mut app)) = reader(1, Theme::dark()) else {
        return;
    };
    hover_until_read(&mut app, 1);
    eprintln!("phase double_click_selects_the_word");
    let quick = glyph_at(&app, 1, "quick", 2);
    Framed::harness_mut(&mut app).double_click_at(quick.x, quick.y);
    assert_eq!(selected(&app), "quick");

    eprintln!("phase triple_click_selects_the_line");
    let brown = glyph_at(&app, 1, "Page 1 line 5:", 30);
    Framed::harness_mut(&mut app).triple_click_at(brown.x, brown.y);
    assert_eq!(selected(&app).trim_end(), line_text(1, 5));

    eprintln!("phase a_click_clears_it");
    let elsewhere = glyph_at(&app, 1, "Page 1 line 9:", 3);
    Framed::harness_mut(&mut app).click_at(elsewhere.x, elsewhere.y);
    assert!(app.selection.is_none(), "a plain click selects nothing");
}

#[serial(pdfium)]
#[test]
fn shift_click_extends_the_selection() {
    let Some((_dir, mut app)) = reader(1, Theme::dark()) else {
        return;
    };
    hover_until_read(&mut app, 1);
    let from = glyph_at(&app, 1, "Page 1 line 1:", 0);
    let to = past(&app, 1, "Page 1 line 1:", 5);
    drag(&mut app, from, to);
    assert_eq!(selected(&app), "Page 1");
    // Well below the selection, clear of its menu.
    let further = past(&app, 1, "Page 1 line 12:", 5);
    let harness = Framed::harness_mut(&mut app);
    harness.set_modifiers(&["shift".to_owned()]);
    harness.click_at(further.x, further.y);
    harness.set_modifiers(&[]);
    let text = selected(&app);
    assert!(
        text.starts_with(&line_text(1, 1)) && text.ends_with("Page 1"),
        "Shift+click moves the selection's end, keeping its start: {text:?}"
    );
    assert!(text.contains(&line_text(1, 7)), "{text:?}");
}

// One lock for both: two stacked locks taken in different orders by
// different tests deadlock.
#[serial(clipboard, pdfium)]
#[test]
fn select_all_then_copy_puts_the_whole_document_on_the_clipboard() {
    let Some((_dir, mut app)) = reader(3, Theme::dark()) else {
        return;
    };
    ui::clipboard_copy("before");
    let harness = Framed::harness_mut(&mut app);
    harness.set_modifiers(&["ctrl".to_owned()]);
    harness.key("a");
    harness.key("c");
    harness.set_modifiers(&[]);
    // Pages off screen are read on the worker; the copy lands with them.
    settle(&mut app, |app| {
        app.status_note
            .as_deref()
            .is_some_and(|note| note.starts_with("Copied"))
    });
    let copied = ui::clipboard_paste().unwrap_or_default();
    if copied == "before" {
        // No clipboard in this session: what would have been copied.
        eprintln!("no clipboard in this session; checking the selection");
    }
    let text = if copied == "before" {
        selected(&app)
    } else {
        copied
    };
    for page in 1..=3 {
        assert!(text.contains(&line_text(page, 0)), "{text:?}");
        assert!(text.contains(&line_text(page, LINES - 1)), "{text:?}");
    }
    let first = text.find(&line_text(1, 0)).unwrap();
    let last = text.find(&line_text(3, 0)).unwrap();
    assert!(first < last, "the pages are copied in order");
}

#[serial(pdfium)]
#[test]
fn a_press_before_the_page_text_arrives_selects_once_it_does() {
    let Some((_dir, mut app)) = reader(1, Theme::dark()) else {
        return;
    };
    hover_until_read(&mut app, 1);
    let from = glyph_at(&app, 1, "Page 1 line 6:", 0);
    let to = past(&app, 1, "Page 1 line 6:", 10);
    // The layer goes, as on a page the pointer never rested on.
    app.raster_text.clear();
    app.text_hover = None;
    app.raster_text_batch.clear();
    drag(&mut app, from, to);
    settle(&mut app, |app| {
        app.selection.is_some_and(|sel| !sel.dragging)
    });
    assert_eq!(
        selected(&app),
        "Page 1 line",
        "the drag made before the text came selects what it covered"
    );
}

/// Press the right button at `at`, as a right-click does.
fn right_click(app: &mut Framed<PdfApp>, at: Point) {
    move_to(app, at);
    for state in [ElementState::Pressed, ElementState::Released] {
        Framed::harness_mut(app).send(&WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state,
            button: MouseButton::Right,
        });
    }
}

/// The selection menu's `action`, once it shows: its button's middle.
fn menu_button(app: &mut Framed<PdfApp>, action: ui::HudAction) -> Point {
    settle(app, |app| app.sel_hud.is_some());
    let hud = app.sel_hud.as_ref().expect("the selection menu shows");
    hud.buttons()
        .iter()
        .find(|(_, a)| *a == action)
        .map_or_else(
            || panic!("the selection menu offers {action:?}"),
            |(rect, _)| rect.center(),
        )
}

/// The actions the selection menu offers.
fn menu_actions(app: &mut Framed<PdfApp>) -> Vec<ui::HudAction> {
    settle(app, |app| app.sel_hud.is_some());
    let hud = app.sel_hud.as_ref().expect("the selection menu shows");
    hud.buttons().iter().map(|(_, a)| *a).collect()
}

// One lock for both: two stacked locks taken in different orders by
// different tests deadlock.
#[serial(clipboard, pdfium)]
#[test]
fn a_right_click_selects_the_word_and_the_selection_menu_copies_and_selects_all() {
    let Some((_dir, mut app)) = reader(1, Theme::dark()) else {
        return;
    };
    hover_until_read(&mut app, 1);
    eprintln!("phase a_right_click_on_a_word_selects_it_under_the_selection_menu");
    let fox = glyph_at(&app, 1, "fox", 1);
    right_click(&mut app, fox);
    assert_eq!(selected(&app), "fox", "a right-click on a word selects it");
    assert!(app.context_menu.is_none(), "one menu: the selection's own");
    let copy = menu_button(&mut app, ui::HudAction::Copy);
    ui::clipboard_copy("before");
    Framed::harness_mut(&mut app).click_at(copy.x, copy.y);
    assert_eq!(
        app.status_note.as_deref(),
        Some("Copied 3 characters"),
        "Copy copied the word"
    );
    let pasted = ui::clipboard_paste().unwrap_or_default();
    assert!(
        pasted == "fox" || pasted == "before",
        "the clipboard holds the word: {pasted:?}"
    );

    eprintln!("phase a_right_click_on_the_selection_keeps_it");
    // Clear of the menu under the word.
    let from = glyph_at(&app, 1, "Page 1 line 12:", 0);
    let to = past(&app, 1, "Page 1 line 13:", 5);
    drag(&mut app, from, to);
    let before = selected(&app);
    assert!(before.starts_with(&line_text(1, 12)), "{before:?}");
    let on_it = glyph_at(&app, 1, "Page 1 line 12:", 20);
    right_click(&mut app, on_it);
    assert_eq!(selected(&app), before, "the selection stays for the menu");

    eprintln!("phase select_all_from_the_menu");
    let all = menu_button(&mut app, ui::HudAction::SelectAll);
    Framed::harness_mut(&mut app).click_at(all.x, all.y);
    let sel = app.selection.expect("everything is selected");
    assert_eq!((sel.start().page, sel.start().glyph), (1, 0));
    assert!(selected(&app).contains(&line_text(1, LINES - 1)));
}

#[serial(pdfium)]
#[test]
fn a_held_finger_selects_a_word_and_its_grips_move_the_ends() {
    // Two pages, so the document is taller than the window and scrolls.
    let Some((_dir, mut app)) = reader(2, Theme::dark()) else {
        return;
    };
    Framed::harness_mut(&mut app)
        .set_interaction_environment(ui::interaction::InteractionEnvironment::TOUCH);
    hover_until_read(&mut app, 1);
    eprintln!("phase a_hold_selects_the_word_and_offers_copy");
    let brown = glyph_at(&app, 1, "Page 1 line 4:", 27);
    Framed::harness_mut(&mut app).touch_hold_at(brown.x, brown.y);
    assert_eq!(selected(&app), "brown", "a held finger selects the word");
    let touch_menu = menu_actions(&mut app);

    eprintln!("phase dragging_the_end_grip_extends_the_selection");
    let grips = app.selection_grips().expect("grips on a touchscreen");
    let knob = grips.knob(ui::widget::TextGrip::End);
    let to = past(&app, 1, "Page 1 line 4:", 39);
    let harness = Framed::harness_mut(&mut app);
    harness.touch(0, TouchPhase::Started, knob.x, knob.y);
    let harness = Framed::harness_mut(&mut app);
    for step in 1..=6 {
        let f = f64::from(step) / 6.0;
        harness.touch(
            0,
            TouchPhase::Moved,
            knob.x + (to.x - knob.x) * f,
            knob.y + (to.y - knob.y) * f,
        );
    }
    harness.touch(0, TouchPhase::Ended, to.x, to.y);
    assert_eq!(selected(&app), "brown fox jumps");

    eprintln!("phase dragging_the_start_grip_moves_the_start");
    let grips = app.selection_grips().expect("grips on a touchscreen");
    let knob = grips.knob(ui::widget::TextGrip::Start);
    let to = glyph_at(&app, 1, "Page 1 line 4:", 15);
    let harness = Framed::harness_mut(&mut app);
    harness.touch(0, TouchPhase::Started, knob.x, knob.y);
    for step in 1..=6 {
        let f = f64::from(step) / 6.0;
        harness.touch(
            0,
            TouchPhase::Moved,
            knob.x + (to.x - knob.x) * f,
            knob.y + (to.y - knob.y) * f,
        );
    }
    harness.touch(0, TouchPhase::Ended, to.x, to.y);
    assert_eq!(selected(&app), "the quick brown fox jumps");

    eprintln!("phase a_finger_dragged_on_the_page_still_scrolls");
    let before = app.scroll_y();
    let at = glyph_at(&app, 1, "Page 1 line 20:", 3);
    Framed::harness_mut(&mut app).swipe_at((at.x, at.y), (at.x, at.y - 200.0));
    assert!(app.scroll_y() > before, "a swipe on text scrolls");
    assert_eq!(selected(&app), "the quick brown fox jumps");

    eprintln!("phase a_pointer_gets_the_same_menu");
    Framed::harness_mut(&mut app)
        .set_interaction_environment(ui::interaction::InteractionEnvironment::POINTER);
    let fox = glyph_at(&app, 1, "Page 1 line 22:", 32);
    right_click(&mut app, fox);
    assert_eq!(selected(&app), "fox");
    assert_eq!(
        menu_actions(&mut app),
        touch_menu,
        "one selection menu, the same for a finger and a pointer"
    );
    assert!(
        touch_menu.contains(&ui::HudAction::SelectAll) && touch_menu.contains(&ui::HudAction::Note)
    );
}

// One lock for both: two stacked locks taken in different orders by
// different tests deadlock.
#[serial(clipboard, pdfium)]
#[test]
fn agents_select_and_copy_text_through_actions() {
    use crate::actions::PdfEffect;
    let dir = tempfile::tempdir().expect("temp dir");
    let path = text_pdf(dir.path(), 3);
    let mut app = super::tests::launched(&path);
    eprintln!("phase select_text_on_a_page_the_screen_does_not_show");
    let answer = app
        .effect(&PdfEffect::SelectText {
            text: Some("quick brown".into()),
            page: Some(3),
        })
        .expect("the text is selected")
        .value
        .expect("a selection answers");
    assert_eq!(answer["page"], 3);
    assert_eq!(answer["text"], "quick brown");
    let shown = app
        .with_surface::<PdfApp, _>(|pdf| (pdf.current_page, pdf.selection_text()))
        .expect("the reader");
    assert_eq!(shown, (3, Some("quick brown".to_owned())));

    eprintln!("phase copy_answers_with_the_copied_text");
    let copied = app
        .effect(&PdfEffect::CopySelection)
        .expect("the selection copies")
        .value
        .expect("a copy answers");
    assert_eq!(copied["text"], "quick brown");

    eprintln!("phase text_that_is_not_there_is_refused");
    assert!(
        app.effect(&PdfEffect::SelectText {
            text: Some("not in this document".into()),
            page: Some(1),
        })
        .is_err()
    );

    eprintln!("phase a_whole_page_and_select_all");
    let answer = app
        .effect(&PdfEffect::SelectText {
            text: None,
            page: Some(2),
        })
        .expect("the page is selected")
        .value
        .expect("a selection answers");
    let text = answer["text"].as_str().unwrap();
    assert!(text.contains(&line_text(2, 0)) && text.contains(&line_text(2, LINES - 1)));
    app.effect(&PdfEffect::SelectAll).expect("select all");
    let copied = app
        .effect(&PdfEffect::CopySelection)
        .expect("everything copies")
        .value
        .expect("a copy answers");
    let text = copied["text"].as_str().unwrap();
    assert!(text.contains(&line_text(1, 0)) && text.contains(&line_text(3, LINES - 1)));
}

/// Screenshots of a selection across lines, with the selection menu, in
/// dark and light, rendered as the desktop renders without a GPU: written
/// where `PDF_SELECTION_SHOTS` names.
#[serial(pdfium)]
#[test]
fn shoot_selection() {
    let Some(out) = std::env::var_os("PDF_SELECTION_SHOTS") else {
        return;
    };
    let out = std::path::PathBuf::from(out);
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let Some((_dir, mut app)) = reader(2, theme) else {
            return;
        };
        hover_until_read(&mut app, 1);
        let from = glyph_at(&app, 1, "Page 1 line 3:", 15);
        let to = past(&app, 1, "Page 1 line 5:", 34);
        drag(&mut app, from, to);
        move_to(&mut app, Point::new(900.0, 700.0));
        settle(&mut app, |app| !app.pending_visuals);
        write_png(&app, &out.join(format!("selection-{name}.png")));
        let quick = glyph_at(&app, 1, "Page 1 line 8:", 22);
        Framed::harness_mut(&mut app)
            .set_interaction_environment(ui::interaction::InteractionEnvironment::TOUCH);
        Framed::harness_mut(&mut app).touch_hold_at(quick.x, quick.y);
        // The menu's opening finishes.
        let opened = ui::time::now();
        settle(&mut app, |app| {
            !app.pending_visuals && opened.elapsed() > Duration::from_millis(600)
        });
        write_png(&app, &out.join(format!("selection-touch-{name}.png")));
    }
}

fn write_png(app: &Framed<PdfApp>, path: &std::path::Path) {
    use image::ImageEncoder as _;
    let (w, h) = (1000u32, 800u32);
    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    {
        let mut target = tilo_cpu_raster::Target::new(&mut pixels, w, h, w as usize * 4)
            .expect("a frame-sized target");
        tilo_cpu_raster::CpuRasterizer::default().render(
            Framed::harness(app).scene().encoding(),
            &mut target,
            None,
            tilo_cpu_raster::color::AlphaColor::from_rgba8(128, 128, 128, 255),
        );
    }
    let file = std::fs::File::create(path).expect("create the shot");
    image::codecs::png::PngEncoder::new(std::io::BufWriter::new(file))
        .write_image(&pixels, w, h, image::ExtendedColorType::Rgba8)
        .expect("write the shot");
}

#[serial(pdfium)]
#[test]
fn a_drag_held_at_the_canvas_edge_scrolls_and_keeps_selecting() {
    let Some((_dir, mut app)) = reader(3, Theme::dark()) else {
        return;
    };
    hover_until_read(&mut app, 1);
    let from = glyph_at(&app, 1, "Page 1 line 2:", 0);
    move_to(&mut app, from);
    Framed::harness_mut(&mut app).send(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Pressed,
        button: MouseButton::Left,
    });
    let canvas = app.canvas_rect;
    let edge = Point::new(from.x, canvas.y1 - 4.0);
    move_to(&mut app, edge);
    let before = app.scroll_y();
    // The hold lasts a fixed number of 60 Hz frames on the frame clock, so
    // how far it scrolls is the same on a loaded machine: far enough to
    // bring the third page, whose text is not read yet, under the pointer.
    let mut frame_clock = ui::time::now();
    for _ in 0..HELD_FRAMES {
        frame_clock += FRAME;
        ui::time::testing::with_monotonic_time(frame_clock, || {
            Framed::harness_mut(&mut app).relayout();
        });
    }
    let scrolled = app.scroll_y() - before;
    let at_bottom = app.page_near(edge).unwrap();
    assert_eq!(at_bottom, 3, "the hold scrolled the third page under it");
    // Still held, with the clock still, until that page's text is read:
    // the selection reaches it as it arrives.
    let deadline = tilo_test_env::hang_deadline();
    while !app.raster_text.contains_key(&at_bottom) {
        assert!(
            ui::time::now() < deadline,
            "page {at_bottom}'s text never arrived"
        );
        std::thread::sleep(Duration::from_millis(5));
        ui::time::testing::with_monotonic_time(frame_clock, || {
            Framed::harness_mut(&mut app).relayout();
        });
    }
    let still = app.scroll_y() - before;
    assert!(
        (still - scrolled).abs() < 1e-9,
        "a still clock scrolls no further: {scrolled}px, then {still}px"
    );
    Framed::harness_mut(&mut app).send(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    });
    assert!(scrolled > 50.0, "holding at the edge scrolls: {scrolled}px");
    let sel = app.selection.expect("the drag selected text");
    assert_eq!(
        sel.end().page,
        at_bottom,
        "the selection followed the scroll"
    );
    assert!(selected(&app).starts_with(&line_text(1, 2)));
}

/// A frame of a 60 Hz host.
const FRAME: Duration = Duration::from_micros(16_667);

/// How many frames [`a_drag_held_at_the_canvas_edge_scrolls_and_keeps_selecting`]
/// holds at the edge: 0.8 s, well past where the third page comes under
/// the pointer (about 0.5 s at the edge's speed).
const HELD_FRAMES: usize = 48;
