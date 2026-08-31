//! Browser stub `AppWidget` living inside the nirios shell.
//!
//! Explicitly *not* a real browser port — the shell uses this placeholder
//! tile until/unless Servo is revived.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::shell::browser_stub::BrowserStubWidget;
use std::sync::{Arc, Mutex};
use ui::{AppKind, AppWidget, IconDescriptor};

#[test]
fn browser_stub_advertises_itself_as_browser_app() {
    let w = BrowserStubWidget::new();
    assert_eq!(w.app_kind(), AppKind::Browser);
    assert_eq!(w.app_title(), "Browser");
    match w.app_icon() {
        IconDescriptor::Glyph(g) => assert_eq!(g, "󰖟"),
        IconDescriptor::Named(name) => panic!("glyph expected, got Named({name:?})"),
    }
    assert_eq!(w.permission_hint(), Some(AppKind::Browser));
}

#[test]
fn browser_stub_open_external_invokes_runner_with_url() {
    let calls: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let calls_cloned = Arc::clone(&calls);
    let mut w = BrowserStubWidget::with_runner(Box::new(move |url: &str| {
        calls_cloned.lock().unwrap().push(url.to_string());
    }));

    w.open_external("https://example.com");

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &["https://example.com".to_string()]
    );
    assert_eq!(w.last_url(), Some("https://example.com"));
}
