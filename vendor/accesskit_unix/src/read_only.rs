// Tilo addition to the accesskit_unix 0.23.0 release; see PROVENANCE.md and
// vendor/PATCHES.md, "Accessibility and YouTube manifests".

//! Whether assistive clients on the AT-SPI bus may act on this process's
//! accessible trees, or only read them.
//!
//! An AT-SPI method call carries no proof of a person behind it: any process
//! that reaches the bus can press a button, set a value or type into a field,
//! and the display server never sees an input event. Tilo's launcher sets
//! [`READ_ONLY_ENV`] to `1` in a sandboxed app's environment, and then every
//! method that acts on the tree is refused before it reaches the app's
//! action handler. Reading stays open: names, roles, states, text, values,
//! children and events, which is everything a screen reader needs. Without
//! the variable the crate behaves as the release does, and its public API is
//! the release's.

use zbus::fdo;

/// The variable that turns the read-only tree on, when it holds `1`.
pub(crate) const READ_ONLY_ENV: &str = "TILO_ATSPI_READ_ONLY";

/// The gate every acting AT-SPI method passes first: `Action.DoAction`,
/// every `EditableText` method, setting `Value.CurrentValue`, the
/// `Selection` methods that select or deselect, `Component.GrabFocus`,
/// moving the caret or a text selection through `Text`, and
/// `Document.SetTextSelections`.
///
/// Read on each call rather than once: acting calls are rare, and the value
/// is the one the process was started with.
pub(crate) fn admit() -> fdo::Result<()> {
    if std::env::var_os(READ_ONLY_ENV).is_some_and(|value| value == "1") {
        Err(fdo::Error::AccessDenied(
            "this application's accessible tree is read-only to assistive clients".into(),
        ))
    } else {
        Ok(())
    }
}
