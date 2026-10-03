//! User-facing text, as fluent keys.
//!
//! Every string the user reads in the editor chrome is looked up by key
//! rather than written at the call site, so the app translates without
//! touching the layout code and `--pseudo-locale` exercises every label.
//! The bundle is registered once per thread by [`install`]; it is embedded
//! with `include_str!` (as `photos`, `chat` and `word-processor` do), so a
//! `--shots` render and a unit test resolve the same strings a live session
//! does. Keys are `slides-*` so they cannot collide with another app's
//! resources in the shared registry.

use std::cell::RefCell;
use std::collections::HashMap;

use ui::i18n::{ArgValue, I18nManager, PseudoLocale};

/// The English bundle.
const EN_US: &str = include_str!("../locales/en-US.ftl");

/// Register the bundle. Idempotent, and safe to call from both the live
/// binary and a test.
pub fn install() {
    I18nManager::init("en-US");
    I18nManager::add_resource("en-US", EN_US.to_string());
}

/// Look up `key`. Registers the bundle on first use so a unit test that
/// constructs the app directly still gets real text rather than a raw key.
///
/// The chrome asks for the same few dozen labels on every `compute_chrome`
/// (each frame, pointer move and press), so resolved text is memoised per
/// thread and dropped whenever the manager's revision or the pseudo-locale
/// mode changes — a lookup is then a hash probe and a clone, not a fluent
/// resolve.
pub fn tr(key: &str) -> String {
    ensure();
    thread_local! {
        static CACHE: RefCell<(u64, PseudoLocale, HashMap<String, String>)> =
            RefCell::new((0, PseudoLocale::Off, HashMap::new()));
    }
    let revision = I18nManager::revision();
    let pseudo = I18nManager::pseudo_locale();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.0 != revision || cache.1 != pseudo {
            *cache = (revision, pseudo, HashMap::new());
        }
        if let Some(text) = cache.2.get(key) {
            return text.clone();
        }
        let text = I18nManager::lookup(key, None);
        cache.2.insert(key.to_owned(), text.clone());
        text
    })
}

/// Look up `key`, interpolating `args` into its placeables.
pub fn tr_args(key: &str, args: &[(&str, ArgValue)]) -> String {
    ensure();
    I18nManager::format(key, args)
}

/// Install the bundle once **per thread**: `I18nManager` keeps its bundles
/// in a `thread_local!`, so a process-wide `Once` would leave every other
/// thread looking up an empty manager and reading keys instead of sentences.
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

    /// Every chrome key resolves and formatted keys interpolate their arguments.
    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "cases merged from separate tests; each case stays self-contained"
    )]
    fn chrome_strings_resolve_and_interpolate() {
        // Case `every_key_the_chrome_uses_resolves`.
        // Every key the chrome asks for resolves to real text. A missing key
        // comes back as the key itself, which would ship as `slides-present` on
        // a button face.
        {
            for key in [
                "slides-present",
                "slides-show-slides",
                "slides-show-properties",
                "slides-slides",
                "slides-hide-slides",
                "slides-drop-to-delete",
                "slides-tool-text",
                "slides-tool-rectangle",
                "slides-tool-ellipse",
                "slides-tool-line",
                "slides-tool-image",
                "slides-hide-properties",
                "slides-fill",
                "slides-stroke",
                "slides-font",
                "slides-alignment",
                "slides-arrange",
                "slides-layout",
                "slides-background",
                "slides-deck-theme",
                "slides-speaker-notes",
                "slides-notes-placeholder",
                "slides-custom-colour",
                "slides-fill-colour",
                "slides-bold",
                "slides-italic",
                "slides-bring-front",
                "slides-send-back",
                "slides-duplicate",
                "slides-delete",
                "slides-colour-none",
                "slides-colour-accent",
                "slides-colour-accent-soft",
                "slides-colour-surface",
                "slides-colour-deep",
                "slides-colour-text",
                "slides-colour-white",
                "slides-colour-green",
                "slides-colour-amber",
                "slides-colour-red",
                "slides-colour-violet",
                "slides-image-loading",
                "slides-image-unavailable",
                "slides-current",
                "slides-next",
                "slides-end-of-deck",
                "slides-no-notes",
                "slides-presenter-keys",
                "slides-shortcuts-title",
                "slides-shortcuts-editing",
                "slides-shortcuts-presenting",
                "slides-key-new-slide",
                "slides-key-exit",
                "slides-a11y-new-slide",
                "slides-a11y-duplicate-slide",
                "slides-a11y-delete-slide",
                "slides-a11y-start-presentation",
                "slides-a11y-slide-overview",
                "slides-a11y-custom-fill",
                "slides-a11y-bold",
                "slides-a11y-italic",
                "slides-a11y-bring-front",
                "slides-a11y-send-back",
                "slides-a11y-duplicate-element",
                "slides-a11y-delete-element",
                "slides-a11y-edit-notes",
                "slides-a11y-slides-panel",
                "slides-a11y-properties-panel",
                "slides-a11y-more-tools",
                "slides-toast-open-failed",
                "slides-toast-import-failed",
                "slides-toast-export-failed",
                "slides-toast-chooser-failed",
                "slides-toast-export-limit",
                "slides-toast-opened",
                "slides-toast-saved",
                "slides-toast-process-failed",
                "slides-toast-image-chooser-failed",
                "slides-toast-image-import-failed",
                "slides-toast-opening-pptx",
                "slides-toast-shared-limit",
                "slides-pick-image-title",
                "slides-pick-image-accept",
                "slides-pick-images-filter",
                "slides-open-title",
                "slides-open-accept",
                "slides-save-title",
                "slides-save-accept",
                "slides-pick-presentations-filter",
            ] {
                let text = tr(key);
                assert_ne!(text, key, "{key} has no English text");
                assert!(!text.is_empty(), "{key} resolves to nothing");
            }
        }

        // Case `formatted_keys_interpolate_their_arguments`.
        // Placeables interpolate: the presenter position line carries the
        // slide and the total, the custom colour its hex.
        {
            let pos = tr_args(
                "slides-position",
                &[
                    ("current", ArgValue::from(2_usize)),
                    ("total", ArgValue::from(5_usize)),
                ],
            );
            assert_eq!(pos, "Slide 2 of 5");
            let custom = tr_args(
                "slides-custom-colour-hex",
                &[("hex", ArgValue::from("#ff0000"))],
            );
            assert_eq!(custom, "Custom colour #ff0000");
        }
    }
}
