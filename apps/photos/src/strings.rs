//! User-facing text, as fluent keys.
//!
//! Every string the user reads is looked up by key rather than written at the
//! call site, so the app translates without touching the widget code. The
//! lookups go through the app's declared catalog, which loads itself on first
//! use, so a unit test that constructs a widget directly gets real text.

/// Look up `key`.
pub fn tr(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

/// Look up `key`, interpolating `args` into its placeables. A number
/// argument also picks the message's plural form.
pub fn tr_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::resource::CATALOG.t_args(key, args)
}

/// Look up `key` with one text argument.
pub fn tr_with(key: &str, name: &str, value: &str) -> String {
    tr_args(key, &[(name, ui::i18n::ArgValue::Text(value.to_owned()))])
}

/// Look up `key` with one count, which also picks the plural form.
pub fn tr_count(key: &str, count: usize) -> String {
    tr_args(key, &[("count", ui::i18n::ArgValue::Number(count as f64))])
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
            "photos-command-crop",
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
            "photos-command-rotate-left",
            "photos-command-rotate-right",
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

    /// Every key the sources look up exists in the bundle: a key spelled at
    /// a call site but missing from `en-US.ftl` would ship as its own name.
    #[test]
    fn every_key_in_the_sources_is_in_the_bundle() {
        let sources = [
            include_str!("app.rs"),
            include_str!("app/chrome.rs"),
            include_str!("analysis.rs"),
            include_str!("import.rs"),
            include_str!("lib.rs"),
            include_str!("people.rs"),
            include_str!("resource.rs"),
            include_str!("search.rs"),
        ];
        let mut checked = 0;
        for source in sources {
            for call in ["tr(\"", "tr_with(\"", "tr_count(\"", "tr_args(\n"] {
                for (at, _) in source.match_indices(call) {
                    let rest = source.get(at + call.len() - 1..).unwrap_or_default();
                    let Some(open) = rest.find("\"photos-") else {
                        continue;
                    };
                    let key_start = open + 1;
                    let Some(len) = rest.get(key_start..).and_then(|tail| tail.find('"')) else {
                        continue;
                    };
                    let key = &rest[key_start..key_start + len];
                    if key.contains('{') || open > 40 {
                        continue;
                    }
                    assert_ne!(tr(key), key, "`{key}` is not in the bundle");
                    checked += 1;
                }
            }
        }
        assert!(checked > 200, "the scan found only {checked} keys");
    }
}
