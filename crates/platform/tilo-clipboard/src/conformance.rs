//! The contract every [`ClipboardBackend`] keeps, as checks a backend's tests
//! (and a host's own backend's tests) run.
//!
//! The checks panic with the violated rule. They write the clipboard they are
//! given, so run [`check_system`] only where clobbering it is acceptable.
#![allow(
    clippy::missing_panics_doc,
    reason = "a conformance check fails by panicking with the violated rule"
)]

use crate::{Availability, ClipboardBackend, ClipboardTarget, Feature, availability, available};

/// A backend that keeps everything in the process ([`InProcessClipboard`](crate::InProcessClipboard)
/// or a test double): round trips, independent targets, and no false claim of
/// a shared copy or an image.
pub fn check_local(backend: &dyn ClipboardBackend) {
    round_trips(backend);
    rich_round_trips(backend);
    targets_are_independent(backend);
    assert!(
        !backend.set_shared_text(ClipboardTarget::Clipboard, "secret"),
        "{}: a clipboard private to the process must not claim a shared copy",
        backend.name()
    );
    assert!(
        !backend.set_secret_text("secret"),
        "{}: a clipboard private to the process must not claim a secret copy",
        backend.name()
    );
    assert!(
        !backend.set_image(1, 1, &[0; 4]),
        "{}: a text-only clipboard must refuse images",
        backend.name()
    );
    names_itself(backend);
}

/// This target's [`system`](crate::system) backend, with or without a real
/// clipboard behind it: text round-trips inside the process either way, a
/// malformed image is refused, and a target the column marks unsupported
/// never leaves the app.
pub fn check_system(backend: &dyn ClipboardBackend) {
    if available(Feature::ReadAnytime) {
        round_trips(backend);
    }
    assert!(
        !backend.set_image(2, 2, &[0; 4]),
        "{}: an image whose size does not match its pixels must be refused",
        backend.name()
    );
    if matches!(
        availability(Feature::PrimarySelection),
        Availability::Unsupported(_)
    ) {
        assert!(
            !backend.set_shared_text(ClipboardTarget::Primary, "selection"),
            "{}: a primary selection this target lacks must not be reported as shared",
            backend.name()
        );
    }
    names_itself(backend);
}

fn round_trips(backend: &dyn ClipboardBackend) {
    let payload = "Tilo — café 👋 https://example.com/feed.xml";
    assert!(
        backend.set_text(ClipboardTarget::Clipboard, payload),
        "{}: copy must succeed, if only inside the process",
        backend.name()
    );
    assert_eq!(
        backend.text(ClipboardTarget::Clipboard).as_deref(),
        Some(payload),
        "{}: paste must return what was copied",
        backend.name()
    );
}

/// A formatted copy pastes its formatting where it is read, its words
/// everywhere else, and a later plain copy leaves no stale formatting.
fn rich_round_trips(backend: &dyn ClipboardBackend) {
    let html = "<p>plain <b>bold</b></p>";
    assert!(
        backend.set_rich_text(html, "plain bold"),
        "{}: a formatted copy must succeed, if only inside the process",
        backend.name()
    );
    assert_eq!(
        backend.text(ClipboardTarget::Clipboard).as_deref(),
        Some("plain bold"),
        "{}: a formatted copy pastes its words as plain text",
        backend.name()
    );
    let rich = backend.rich_text();
    assert_eq!(
        rich.as_ref().map(|rich| rich.html.as_str()),
        Some(html),
        "{}: a formatted copy pastes its formatting",
        backend.name()
    );
    assert_eq!(
        rich.and_then(|rich| rich.plain).as_deref(),
        Some("plain bold")
    );
    assert!(backend.set_text(ClipboardTarget::Clipboard, "later"));
    assert!(
        backend.rich_text().is_none(),
        "{}: a plain copy replaces the formatting of the one before",
        backend.name()
    );
}

/// The two targets must not alias. This is the property that made the
/// terminal's middle-click paste return the Ctrl+C text instead of the
/// drag-selected one.
fn targets_are_independent(backend: &dyn ClipboardBackend) {
    assert!(backend.set_text(ClipboardTarget::Clipboard, "copied"));
    assert!(backend.set_text(ClipboardTarget::Primary, "selected"));
    assert_eq!(
        backend.text(ClipboardTarget::Clipboard).as_deref(),
        Some("copied"),
        "{}: the primary selection overwrote the clipboard",
        backend.name()
    );
    assert_eq!(
        backend.text(ClipboardTarget::Primary).as_deref(),
        Some("selected"),
        "{}: the clipboard overwrote the primary selection",
        backend.name()
    );
}

fn names_itself(backend: &dyn ClipboardBackend) {
    assert!(
        !backend.name().is_empty(),
        "a backend names itself for diagnostics"
    );
}
