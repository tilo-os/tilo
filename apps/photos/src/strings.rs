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
            "photos-add-to-album",
            "photos-album-editor-hint",
            "photos-album-empty-body",
            "photos-album-empty-title",
            "photos-albums",
            "photos-albums-empty-body",
            "photos-albums-empty-title",
            "photos-analyze",
            "photos-app",
            "photos-cancel",
            "photos-captured",
            "photos-clear-selection",
            "photos-confirm-delete-album",
            "photos-confirm-remove",
            "photos-copied",
            "photos-copy",
            "photos-copy-failed",
            "photos-crop",
            "photos-crop-noop",
            "photos-delete-album",
            "photos-description",
            "photos-done",
            "photos-edit-album",
            "photos-edit-details",
            "photos-edited",
            "photos-edited-state",
            "photos-empty-body",
            "photos-empty-title",
            "photos-error-body",
            "photos-error-title",
            "photos-favorite",
            "photos-favorites",
            "photos-favorites-empty-body",
            "photos-favorites-empty-title",
            "photos-filter-empty-body",
            "photos-filter-empty-title",
            "photos-filter-images",
            "photos-folder",
            "photos-image",
            "photos-image-help",
            "photos-larger-thumbnails",
            "photos-latitude",
            "photos-library",
            "photos-library-empty-body",
            "photos-library-empty-title",
            "photos-loading-body",
            "photos-loading-title",
            "photos-longitude",
            "photos-new-album",
            "photos-open",
            "photos-open-failed",
            "photos-open-photo",
            "photos-open-timeline",
            "photos-open-timeline-day",
            "photos-open-timeline-month",
            "photos-open-title",
            "photos-organize",
            "photos-people",
            "photos-photo-details",
            "photos-photographs",
            "photos-place",
            "photos-places",
            "photos-places-empty-body",
            "photos-places-empty-title",
            "photos-places-list",
            "photos-remove",
            "photos-reset",
            "photos-rotate-left",
            "photos-rotate-right",
            "photos-save",
            "photos-save-changes",
            "photos-saved",
            "photos-save-failed",
            "photos-select",
            "photos-select-all",
            "photos-select-photos",
            "photos-share",
            "photos-share-copied",
            "photos-share-failed",
            "photos-smaller-thumbnails",
            "photos-tags",
            "photos-time-and-place",
            "photos-timeline",
            "photos-timeline-day-description",
            "photos-timeline-description",
            "photos-timeline-month-description",
            "photos-tools",
            "photos-unfavorite",
            "photos-zoom-in",
            "photos-zoom-out",
        ] {
            let got = tr(key);
            assert_ne!(got, key, "`{key}` is not in the bundle");
            assert!(!got.is_empty(), "`{key}` resolved to nothing");
        }
    }
}
