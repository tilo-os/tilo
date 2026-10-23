//! The window's loading line follows what the page is doing, and the
//! browser draws a line of its own only outside a Tilo session.

use super::*;
use ui::webview::WebViewEngine;

#[test]
fn the_window_loading_line_follows_page_loads_navigations_and_reloads() {
    use ui::window_loading::WindowLoading;
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine.clone(), true);
    let doc = AutoCommit::new();
    let root = automerge::ROOT;
    widget
        .perform_browser_operation(BrowserOperation::Navigate {
            url: "https://a.test".into(),
        })
        .unwrap();
    assert_eq!(
        widget.window_loading(),
        WindowLoading::Busy,
        "a navigation is busy until the engine reports progress"
    );
    // The test engine answers a navigation at once; hold its answer back
    // to see the load in between.
    let _ = engine.drain_events();
    widget.set_loading_progress(0.4);
    widget.sync(&doc, &root);
    assert_eq!(
        ui::window_loading::current(),
        WindowLoading::progress(0.4),
        "the engine's progress reaches the window's line"
    );
    engine.push_event(WebViewEvent::LoadFinished);
    widget.sync(&doc, &root);
    assert_eq!(ui::window_loading::current(), WindowLoading::Idle);
    widget
        .perform_browser_operation(BrowserOperation::Reload)
        .unwrap();
    let _ = engine.drain_events();
    widget.sync(&doc, &root);
    assert_eq!(
        ui::window_loading::current(),
        WindowLoading::Busy,
        "a reload loads again"
    );
    engine.push_event(WebViewEvent::LoadProgress(0.7));
    widget.sync(&doc, &root);
    assert_eq!(ui::window_loading::current(), WindowLoading::progress(0.7));
    engine.push_event(WebViewEvent::LoadFinished);
    widget.sync(&doc, &root);
    assert_eq!(ui::window_loading::current(), WindowLoading::Idle);
}

/// In a Tilo session the desktop draws the window's loading line under
/// its bar; the browser draws no line of its own and asks for no frames
/// for one.
#[test]
fn a_hosted_browser_leaves_its_loading_line_to_the_desktop() {
    let engine = TestEngine::new();
    let mut widget = BrowserWidget::with_engine(engine, true);
    widget
        .perform_browser_operation(BrowserOperation::Navigate {
            url: "https://a.test".into(),
        })
        .unwrap();
    assert!(widget.own_loading_face().is_some(), "a window of its own");
    ui::window_toolbar::set_hosted(true);
    let hosted_face = widget.own_loading_face().is_some();
    let hosted_frames = widget.needs_animation_frame();
    ui::window_toolbar::set_hosted(false);
    assert!(!hosted_face, "the desktop draws the line");
    assert!(!hosted_frames, "and animates it");
    assert!(widget.loading_face().is_some(), "the load is still known");
}
