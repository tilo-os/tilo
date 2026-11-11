//! The empty window's words and its tab names, as a person meets them: the
//! state it says, the failure it explains, and the tabs a narrow panel fits.

use ui::Widget;

use super::tests::{any_access_node, app_with_state, framed, full_frame, staged_empty};
use crate::PdfApp;

/// The empty state names its own state in the window's language, and the
/// words it paints are the words it reads out.
#[test]
fn the_empty_state_says_its_state_in_the_window_language() {
    let catalog = &crate::CATALOG;
    catalog.set_locale("de-DE");
    let app = staged_empty("empty");
    let app = framed(app, crate::scenarios::record_state(), 1280.0, 800.0, None);
    let heading = crate::t("pdf-empty-heading");
    let open = crate::t("pdf-empty-open");
    assert_eq!(heading, "Kein Dokument geöffnet", "German's own heading");
    assert_eq!(open, "PDF öffnen", "German's own action");
    let tree = app.accessibility().expect("empty-state semantics");
    assert!(
        any_access_node(&tree, &|node| node.name.as_deref()
            == Some(heading.as_str())),
        "the heading the person sees is the one read out"
    );
    assert!(
        any_access_node(&tree, &|node| {
            node.name.as_deref() == Some(open.as_str())
                && node.action_target.as_deref() == Some("pdf.open")
        }),
        "the button is named by the message it is painted from"
    );
    catalog.set_locale("en-US");
}

/// Why an open failed is said in the person's words, and the words name
/// what actually failed: the engine's own reasons, a file window that
/// could not open, and a file that is not a PDF each say their own thing.
#[test]
fn a_failed_open_says_the_cause_the_engine_reported() {
    let mut app = PdfApp::empty();
    assert_eq!(app.open_failure_text(), None, "nothing failed yet");
    for (error, key) in [
        (
            "Could not open a.pdf: the PDF is password protected",
            "pdf-empty-detail-password",
        ),
        (
            "Could not open a.pdf: the file is not a readable PDF",
            "pdf-empty-detail-unreadable",
        ),
        (
            "Could not open a.pdf: the file is unavailable or is not a readable PDF",
            "pdf-empty-detail-unreadable",
        ),
        (
            "Could not open a.pdf: PDF engine unavailable: no usable libpdfium.so",
            "pdf-empty-detail-engine",
        ),
        (
            "Could not open a.pdf: the PDF worker could not be started",
            "pdf-empty-detail-engine",
        ),
        (
            "Couldn't open the file chooser: no file window answered",
            "pdf-empty-detail-picker",
        ),
        (
            "Could not open a.pdf: No such file or directory (os error 2)",
            "pdf-empty-detail-other",
        ),
    ] {
        app.load_error = Some(error.to_string());
        assert_eq!(
            app.open_failure_text().as_deref(),
            Some(crate::t(key).as_str()),
            "{error}"
        );
    }
}

/// The review panel's tabs are painted from the catalogue, and the names
/// that end up painted fit the strip: the full names in a wide window,
/// and the short forms where the panel floats over a narrow one.
#[test]
fn the_review_tabs_fit_their_strip_in_every_language() {
    let catalog = &crate::CATALOG;
    let mut measured = Vec::new();
    for locale in ["en-US", "de-DE"] {
        catalog.set_locale(locale);
        for (w, h) in [(1280.0, 800.0), (390.0, 844.0)] {
            let (app, st) = app_with_state();
            let mut app = framed(app, st, w, h, None);
            if app.overlay_mode {
                // A narrow window floats the panel over the pages, as a
                // capture of it keeps it on screen.
                app.show_sidebar = true;
                full_frame(&mut app, w, h, None);
            }
            assert!(app.show_sidebar, "{locale} at {w}: the panel is on screen");
            let widths = app.tab_widths.expect("the tabs are measured");
            let strip = app.sidebar_tab_strip().width();
            let short = app.sidebar_tabs_short();
            let used: f64 = if short { widths[1] } else { widths[0] }.iter().sum();
            measured.push((locale, w, short, used, strip));
        }
    }
    catalog.set_locale("en-US");
    for (locale, w, short, used, strip) in measured {
        assert!(
            used <= strip + 0.5,
            "{locale} at {w}: the painted tab names take {used}px of the {strip}px strip"
        );
        assert_eq!(
            short,
            w < 1000.0,
            "{locale} at {w}: the tab names the strip can hold are painted"
        );
    }
}
