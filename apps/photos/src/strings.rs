//! User-facing text, as fluent keys.
//!
//! Every string the user reads is looked up by key rather than written at the
//! call site, so the app translates without touching the widget code. The
//! bundle is registered once at startup by [`install`]. Embedded with
//! `include_str!`, matching `chat` and `word-processor`: no build step and no
//! runtime file I/O, so a `--shots` render and a unit test resolve the same
//! strings a live session does.

/// The English bundle. Keys are `photos-*` so they cannot collide with another
/// app's resources in the shared registry.
const EN_US: &str = include_str!("../locales/en-US.ftl");

/// Register the bundle. Idempotent, and safe to call from both the live binary
/// and a test.
pub fn install() {
    ui::i18n::I18nManager::init("en-US");
    ui::i18n::I18nManager::add_resource("en-US", EN_US.to_string());
}

/// Look up `key`. Registers the bundle on first use so a unit test that
/// constructs a widget directly still gets real text rather than a raw key.
pub fn tr(key: &str) -> String {
    ensure();
    ui::i18n::I18nManager::lookup(key, None)
}

/// Install the bundle once **per thread**.
///
/// The guard has to be thread-local because what it guards is: `I18nManager`
/// keeps its bundles in a `thread_local!`, so a process-wide `Once` fires on
/// whichever thread happens to be first and leaves every other thread looking
/// up an empty manager — which returns the key itself instead of the sentence.
fn ensure() {
    thread_local! {
        static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    if !DONE.with(|d| d.replace(true)) {
        install();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every key the app asks for must resolve to real text. A missing key
    /// comes back as the key itself, which would ship as `photos-crop` on a
    /// button face.
    #[test]
    fn every_key_the_app_uses_resolves() {
        for key in [
            "photos-open",
            "photos-library",
            "photos-library-empty-title",
            "photos-library-empty-body",
            "photos-open-title",
            "photos-filter-images",
            "photos-rotate-left",
            "photos-rotate-right",
            "photos-crop",
            "photos-reset",
            "photos-copy",
            "photos-save",
            "photos-more",
            "photos-edited",
            "photos-copied",
            "photos-copy-failed",
            "photos-saved",
            "photos-save-failed",
            "photos-open-failed",
            "photos-crop-noop",
            "photos-empty-title",
            "photos-empty-body",
            "photos-loading-title",
            "photos-loading-body",
            "photos-error-title",
            "photos-error-body",
            "photos-app",
            "photos-tools",
            "photos-image",
            "photos-image-help",
        ] {
            let got = tr(key);
            assert_ne!(got, key, "`{key}` is not in the bundle");
            assert!(!got.is_empty(), "`{key}` resolved to nothing");
        }
    }
}
