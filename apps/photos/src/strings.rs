//! User-facing text, as fluent keys. Every string the user reads is looked up by key rather than
//! written at the call site, so the app translates without touching the widget code.

pub fn tr(key: &str) -> String {
    crate::resource::CATALOG.t(key)
}

pub fn tr_args(key: &str, args: &[(&str, ui::i18n::ArgValue)]) -> String {
    crate::resource::CATALOG.t_args(key, args)
}

pub fn tr_with(key: &str, name: &str, value: &str) -> String {
    tr_args(key, &[(name, ui::i18n::ArgValue::Text(value.to_owned()))])
}

pub fn tr_count(key: &str, count: usize) -> String {
    tr_args(key, &[("count", ui::i18n::ArgValue::Number(count as f64))])
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn every_key_resolves_in_the_bundle;

        phase every_key_the_app_uses_resolves;
        {
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
                "photos-save-title",
                "photos-saved-edited",
                "photos-save-cancelled",
                "photos-select",
                "photos-select-all",
                "photos-select-photos",
                "photos-share",
                "photos-share-copied",
                "photos-share-failed",
                "photos-smaller-thumbnails",
                "photos-state-not-kept",
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
}
