//! User-facing text, as fluent keys.
//!
//! Every string the user reads is looked up by key rather than written at the
//! call site, so the app translates without touching the widget code. The
//! bundle is registered once at startup by [`install`].

/// The English bundle. Keys are `photos-*` so they cannot collide with another
/// app's resources in the shared registry.
const EN_US: &str = r#"
photos-open = Open
photos-open-title = Open a photograph
photos-filter-images = Images
photos-rotate-left = Rotate left
photos-rotate-right = Rotate right
photos-crop = Crop to view
photos-reset = Reset
photos-copy = Copy
photos-save = Save
photos-edited = edited
photos-copied = Copied to the clipboard
photos-copy-failed = Could not reach the clipboard
photos-saved = Saved to
photos-save-failed = Could not save
photos-open-failed = That file is not an image this app can read
photos-crop-noop = Zoom in first — cropping to the whole photograph would change nothing
photos-empty-title = No photograph open
photos-empty-body = Open an image to view and adjust it.
photos-remark-sent = Sent to { $to }
"#;

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

/// Look up `key`, interpolating `args`.
///
/// A message that names something — where a remark went — is assembled by
/// `fluent` from the whole sentence rather than by concatenating a fragment
/// onto a value here, because word order is not the same in every language.
pub fn tr_args(key: &str, args: &[(&str, String)]) -> String {
    ensure();
    let args: Vec<(&str, ui::i18n::ArgValue)> = args
        .iter()
        .map(|(k, v)| (*k, ui::i18n::ArgValue::Text(v.clone())))
        .collect();
    ui::i18n::I18nManager::format(key, &args)
}

/// Install the bundle once **per thread**.
///
/// The guard has to be thread-local because what it guards is: `I18nManager`
/// keeps its bundles in a `thread_local!`, so a process-wide `Once` fires on
/// whichever thread happens to be first and leaves every other thread looking
/// up an empty manager — which returns the key itself, so `photos-remark-sent`
/// reaches the status line instead of the sentence.
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
            "photos-open-title",
            "photos-filter-images",
            "photos-rotate-left",
            "photos-rotate-right",
            "photos-crop",
            "photos-reset",
            "photos-copy",
            "photos-save",
            "photos-edited",
            "photos-copied",
            "photos-copy-failed",
            "photos-saved",
            "photos-save-failed",
            "photos-open-failed",
            "photos-crop-noop",
            "photos-empty-title",
            "photos-empty-body",
            "photos-remark-sent",
        ] {
            let got = tr(key);
            assert_ne!(got, key, "`{key}` is not in the bundle");
            assert!(!got.is_empty(), "`{key}` resolved to nothing");
        }
    }
}
