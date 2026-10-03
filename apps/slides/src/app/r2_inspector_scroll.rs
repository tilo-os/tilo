//! The properties inspector's scrolling body, and the fluent-keyed chrome.
//!
//! At 2× on an 800 px-tall window the inspector's sections outgrow the
//! panel. The panel drives the shared [`Scrollable`] by hand —
//! `compute_chrome` shifts and culls the body's items by its offset, the
//! wheel over the panel scrolls it — so every control stays reachable, and
//! nothing scrolled out is painted over the inspector's header.

use winit::event::MouseScrollDelta;

use super::tests::measured_app;
use super::*;

/// A wide window (both panels docked) at `text_scale` with the first
/// element selected.
fn docked_inspector(text_scale: f32) -> SlidesApp {
    let (mut app, _tr) = measured_app(2000.0, text_scale);
    app.sel = app
        .slide()
        .and_then(|s| s.elements.first().map(|e| e.id.clone()));
    app
}

/// The 2x inspector scrolls instead of overflowing; a fitting 1x inspector stays inert.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn inspector_scrolls_only_when_it_overflows() {
    // Case `two_x_inspector_scrolls_instead_of_running_past_the_viewport`.
    {
        let mut app = docked_inspector(2.0);
        let chrome = app.compute_chrome();
        let body = chrome.inspector_body;
        assert_eq!(chrome.inspector_layout, SplitLayout::Docked);
        assert!(
            chrome.inspector_content_h > body.height() + 40.0,
            "the scenario overflows the body: content {:.0} vs viewport {:.0}",
            chrome.inspector_content_h,
            body.height()
        );
        let inside_y = |y: f64| y >= body.y0 - 0.01 && y <= body.y1 + 0.01;
        // Unscrolled: the notes are beyond the viewport, and nothing kept
        // from the body sits outside it.
        let notes = chrome.notes_box.expect("notes box");
        assert!(notes.y1 > body.y1, "notes start below the fold");
        for b in chrome
            .zone_buttons(editor_chrome::Zone::Inspector)
            .filter(|b| !matches!(b.kind, ButtonKind::Notes) && b.rect.y0 >= body.y0)
        {
            assert!(
                inside_y(b.rect.center().y),
                "{} was kept although its centre is outside the body",
                app.semantic_target(&b.hit)
            );
        }

        // Hand the scroll model this frame's extent (as `draw` does) and
        // scroll to the end.
        app.inspector_scroll.set_viewport_rect(body);
        app.inspector_scroll
            .set_content_size(body.width() as f32, chrome.inspector_content_h as f32);
        assert!(app.inspector_scroll.max_offset_y() > 0.0);
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let scrolled = app.compute_chrome();
        let notes = scrolled.notes_box.expect("notes box after scrolling");
        assert!(
            notes.y1 <= body.y1 + 0.01 && notes.y0 >= body.y0 - 0.01,
            "notes {notes:?} are inside the body {body:?} after scrolling to the end"
        );
        let arrange = scrolled
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::BringFront))
            .expect("the arrange actions are reachable after scrolling");
        assert!(inside_y(arrange.rect.center().y));
        // The fill swatches scrolled off the top and were culled.
        assert!(
            !scrolled
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::SetFill(_))),
            "the fill swatches scrolled out and are no longer hit targets"
        );
        // The inspector's own header controls stay put.
        assert!(
            scrolled
                .zone_buttons(editor_chrome::Zone::Inspector)
                .any(|b| matches!(b.hit, Hit::ToggleInspector)),
            "the header keeps its toggle"
        );
        // The font-size field follows the same rule as the buttons.
        assert!(
            chrome.font_input_rect.is_some(),
            "the font row is present unscrolled"
        );
        match scrolled.font_input_rect {
            Some(fi) => assert!(inside_y(fi.center().y), "kept field {fi:?} is inside"),
            None => {
                app.position_inspector_widgets(&scrolled);
                assert!(
                    !app.font_input
                        .get_rect()
                        .contains(scrolled.toolbar.center()),
                    "a culled field keeps no rect under the header row"
                );
            }
        }

        // The wheel over the panel scrolls it and is consumed; at the end it
        // changes nothing and is left alone.
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = body.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(handled, "a wheel over the inspector is consumed");
        assert!(app.inspector_scroll.offset_y() > 0.0, "and scrolls it");
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled, "a wheel past the end is not consumed");
        // Over the canvas the panel never scrolls.
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = scrolled.slide_dest.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
    }

    // Case `one_x_inspector_that_fits_neither_shifts_nor_scrolls`.
    {
        let mut app = docked_inspector(1.0);
        let chrome = app.compute_chrome();
        let body = chrome.inspector_body;
        assert!(
            chrome.inspector_content_h <= body.height() + 0.01,
            "1x content {:.0} fits the {:.0} body",
            chrome.inspector_content_h,
            body.height()
        );
        assert!(
            chrome
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::SetFill(_)))
                && chrome
                    .buttons
                    .iter()
                    .any(|b| matches!(b.hit, Hit::BringFront)),
            "every section is present unscrolled"
        );
        app.inspector_scroll.set_viewport_rect(body);
        app.inspector_scroll
            .set_content_size(body.width() as f32, chrome.inspector_content_h as f32);
        app.last_mouse = body.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
    }
}

/// Every chrome label the app lays out resolves through the fluent bundle:
/// the measured label set carries real text, not keys, and the insert tools
/// read as words.
#[test]
fn chrome_labels_are_fluent_resolved_text() {
    let (app, _tr) = measured_app(1400.0, 1.0);
    for (text, measure) in app.chrome_label_set() {
        assert!(
            !text.starts_with("slides-"),
            "chrome label set carries a raw key: {text}"
        );
        assert!(
            !text.is_empty() || measure == editor_chrome::Measure::FontInput,
            "an empty label is measured"
        );
    }
    let chrome = app.compute_chrome();
    let faces: Vec<String> = chrome
        .buttons
        .iter()
        .filter_map(|b| match &b.kind {
            ButtonKind::Command { label, .. } => Some(label.clone()),
            _ => None,
        })
        .collect();
    assert!(faces.iter().any(|f| f == "Present"), "{faces:?}");
    assert!(faces.iter().any(|f| f == "Rectangle"), "{faces:?}");
    assert!(faces.iter().all(|f| !f.starts_with("slides-")), "{faces:?}");
    for l in &chrome.labels {
        assert!(
            !l.text.starts_with("slides-"),
            "label carries a raw key: {}",
            l.text
        );
    }
}
