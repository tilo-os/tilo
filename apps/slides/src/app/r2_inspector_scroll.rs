//! Round-two regressions for the properties inspector's scroll container
//! (review item L11) and the fluent-keyed chrome (L14).
//!
//! At 2× on an 800 px-tall window the inspector's sections outgrow the
//! panel. Before, they simply ran past the viewport: the ARRANGE row and
//! the speaker notes were painted off-screen and could not be reached. The
//! panel now drives the shared [`Scrollable`] by hand — `compute_chrome`
//! shifts and culls the panel's items by its offset, the wheel over the
//! panel scrolls it — so every control stays reachable.

use winit::event::MouseScrollDelta;

use super::tests::measured_app;
use super::*;

/// The 2x inspector scrolls instead of overflowing; a fitting 1x inspector stays inert.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "cases merged from separate tests; each case stays self-contained"
)]
fn inspector_scrolls_only_when_it_overflows() {
    // Case `two_x_inspector_scrolls_instead_of_running_past_the_viewport`.
    // The inspector in the 2× narrow-inspector scenario overflows, and every
    // control the pointer can see is inside the panel; scrolling to the end
    // brings the last section and the notes box into the viewport, while what
    // scrolled off the top is culled rather than painted over the toolbar.
    {
        let (mut app, _tr) = measured_app(NARROW_BREAKPOINT + 400.0, 2.0);
        app.sel = app
            .slide()
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        let chrome = app.compute_chrome();
        let inspector = chrome.inspector;
        assert!(
            inspector.width() > 100.0,
            "inspector is docked: {inspector:?}"
        );
        assert!(
            chrome.inspector_content_h > inspector.height() + 40.0,
            "the scenario overflows the panel: content {:.0} vs viewport {:.0}",
            chrome.inspector_content_h,
            inspector.height()
        );
        let in_span = |x: f64| x >= inspector.x0 - 0.5 && x <= inspector.x1 + 0.5;
        let inside_y = |y: f64| y >= inspector.y0 - 0.01 && y <= inspector.y1 + 0.01;
        // Unscrolled: the notes box is beyond the viewport (that was the bug),
        // and nothing kept from the panel sits outside it.
        let notes = chrome.notes_box.expect("notes box");
        assert!(
            notes.y1 > inspector.y1,
            "notes start below the fold before scrolling"
        );
        for b in chrome
            .buttons
            .iter()
            .filter(|b| in_span(b.rect.center().x) && b.rect.center().y >= inspector.y0)
        {
            assert!(
                b.rect.center().y <= inspector.y1 + 0.01,
                "{} was kept although its centre is below the panel",
                app.semantic_target(&b.hit)
            );
        }

        // Hand the scroll model this frame's extent (as `draw` does) and scroll
        // to the end.
        app.inspector_scroll.set_viewport_rect(inspector);
        app.inspector_scroll
            .set_content_size(inspector.width() as f32, chrome.inspector_content_h as f32);
        assert!(app.inspector_scroll.max_offset_y() > 0.0);
        app.inspector_scroll.scroll_by(0.0, 1.0e6);
        let scrolled = app.compute_chrome();
        let notes = scrolled.notes_box.expect("notes box after scrolling");
        assert!(
            notes.y1 <= inspector.y1 + 0.01 && notes.y0 >= inspector.y0 - 0.01,
            "notes box {notes:?} is fully inside the panel {inspector:?} after scrolling to the end"
        );
        let arrange = scrolled
            .buttons
            .iter()
            .find(|b| matches!(b.hit, Hit::BringFront))
            .expect("ARRANGE row is reachable after scrolling");
        assert!(
            arrange.rect.y0 >= inspector.y0 - 0.01 && arrange.rect.y1 <= inspector.y1 + 0.01,
            "arrange row {:?} inside {inspector:?}",
            arrange.rect
        );
        // The FILL swatches scrolled off the top and were culled: no panel item
        // has its centre above the panel, so none can be painted over the toolbar
        // by the base-chrome pass (which paints everything `in_inspector` rejects).
        for b in scrolled
            .buttons
            .iter()
            .filter(|b| in_span(b.rect.center().x))
        {
            let toolbar_control = b.rect.center().y < scrolled.toolbar.y1;
            assert!(
                toolbar_control || SlidesApp::in_inspector(&scrolled, b.rect.center()),
                "{} at {:?} is neither toolbar chrome nor inside the panel",
                app.semantic_target(&b.hit),
                b.rect
            );
            if toolbar_control {
                assert!(
                    b.rect.y0 >= scrolled.toolbar.y0 - 0.01
                        && b.rect.y1 <= scrolled.toolbar.y1 + 0.01,
                    "a scrolled-off panel control {} at {:?} leaked into the toolbar band",
                    app.semantic_target(&b.hit),
                    b.rect
                );
            }
        }
        assert!(
            !scrolled
                .buttons
                .iter()
                .any(|b| matches!(b.hit, Hit::SetFill(_))),
            "the FILL swatches scrolled out and are no longer hit targets"
        );
        // The embedded font-size field follows the same rule as the buttons: it
        // had a slot unscrolled, and once its TYPE row leaves the panel it has
        // none — so `position_inspector_widgets` parks it instead of leaving a
        // stale rect in the toolbar band.
        assert!(
            chrome.font_input_rect.is_some(),
            "TYPE row is present unscrolled"
        );
        match scrolled.font_input_rect {
            Some(fi) => assert!(
                inside_y(fi.center().y),
                "kept field {fi:?} is inside the panel"
            ),
            None => {
                app.position_inspector_widgets(&scrolled);
                assert!(
                    !app.font_input
                        .get_rect()
                        .contains(scrolled.toolbar.center()),
                    "a culled field must not keep a rect under the toolbar"
                );
            }
        }
        for l in scrolled
            .labels
            .iter()
            .filter(|l| in_span(l.pos.x) && l.pos.y >= scrolled.toolbar.y1)
        {
            assert!(
                l.pos.y >= inspector.y0 - 0.01,
                "label '{}' leaked above the panel",
                l.text
            );
        }

        // The wheel over the panel scrolls it and is consumed; at the end of the
        // content it changes nothing and is left alone.
        app.inspector_scroll.set_offset_y(0.0);
        app.last_mouse = inspector.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(handled, "wheel over the inspector is consumed");
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
    // A 1× inspector that fits keeps every item where it was and ignores the
    // wheel — the scroll model is inert until there is overflow.
    {
        let (mut app, _tr) = measured_app(NARROW_BREAKPOINT + 400.0, 1.0);
        app.sel = app
            .slide()
            .and_then(|s| s.elements.first().map(|e| e.id.clone()));
        let chrome = app.compute_chrome();
        assert!(
            chrome.inspector_content_h <= chrome.inspector.height() + 0.01,
            "1x content {:.0} fits the {:.0} panel",
            chrome.inspector_content_h,
            chrome.inspector.height()
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
        app.inspector_scroll.set_viewport_rect(chrome.inspector);
        app.inspector_scroll.set_content_size(
            chrome.inspector.width() as f32,
            chrome.inspector_content_h as f32,
        );
        app.last_mouse = chrome.inspector.center();
        let mut handled = false;
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -3.0), 1.0, &mut handled);
        assert!(!handled && app.inspector_scroll.offset_y() == 0.0);
    }
}

/// Every chrome label the app lays out resolves through the fluent bundle:
/// the measured label set carries real text, not keys, and the toolbar's
/// insert tools read as words.
#[test]
fn chrome_labels_are_fluent_resolved_text() {
    let (app, _tr) = measured_app(1400.0, 1.0);
    for (text, _, _) in app.chrome_label_set() {
        assert!(
            !text.starts_with("slides-"),
            "chrome label set carries a raw key: {text}"
        );
        assert!(!text.is_empty());
    }
    let chrome = app.compute_chrome();
    let faces: Vec<String> = chrome
        .buttons
        .iter()
        .filter_map(|b| match &b.kind {
            ButtonKind::Text { label, .. } | ButtonKind::IconText { label, .. } => {
                Some(label.clone())
            }
            _ => None,
        })
        .collect();
    assert!(faces.iter().any(|f| f == "Present"), "{faces:?}");
    assert!(faces.iter().any(|f| f == "Slide"), "{faces:?}");
    assert!(faces.iter().all(|f| !f.starts_with("slides-")), "{faces:?}");
    for l in &chrome.labels {
        assert!(
            !l.text.starts_with("slides-"),
            "label carries a raw key: {}",
            l.text
        );
    }
}
