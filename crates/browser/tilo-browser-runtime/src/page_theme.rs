//! The page's theme, built as a native window's is: from the person's
//! settings (their theme, light or dark, contrast and motion) and from the
//! host's accessibility inputs, which on a page are what the person asked
//! of every page through their browser and system: less motion, more
//! contrast. A change to either draws every mount again in the new theme.

use std::cell::Cell;

/// The media query the browser answers for a person who asked for less
/// motion.
pub(crate) const REDUCED_MOTION: &str = "(prefers-reduced-motion: reduce)";
/// The media query the browser answers for a person who asked for more
/// contrast.
pub(crate) const MORE_CONTRAST: &str = "(prefers-contrast: more)";

thread_local! {
    /// Bumped whenever what the theme is built from changes; a mount that
    /// drew at another generation takes the theme again.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
}

/// The page's accessibility inputs, from which media queries `matches`
/// (the browser's `matchMedia`) holds.
pub(crate) fn accessibility(matches: impl Fn(&str) -> bool) -> ui::host::AccessibilityInputs {
    ui::host::AccessibilityInputs {
        reduce_motion: matches(REDUCED_MOTION),
        high_contrast: matches(MORE_CONTRAST),
        ..ui::host::AccessibilityInputs::default()
    }
}

/// `base` (the theme the settings name) with the host's `accessibility`
/// on top, as a native window applies them.
pub(crate) fn themed(base: ui::Theme, accessibility: ui::host::AccessibilityInputs) -> ui::Theme {
    base.with_a11y(
        &ui::a11y::A11ySettings {
            high_contrast: accessibility.high_contrast,
            reduce_motion: accessibility.reduce_motion,
            ..ui::a11y::A11ySettings::default()
        },
        1.0,
        1.0,
    )
}

/// The theme a mount draws in now.
#[cfg(target_arch = "wasm32")]
pub(crate) fn current() -> ui::Theme {
    themed(
        ui::Theme::configured(),
        ui::host::environment().accessibility,
    )
}

/// Which change of what the theme is built from the page is at.
pub(crate) fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// What the theme is built from changed: every mount takes it again on its
/// next frame.
pub(crate) fn changed() {
    GENERATION.with(|generation| generation.set(generation.get().wrapping_add(1)));
}

/// Take the browser's answers as the page's host inputs, now and whenever
/// the person changes them; `wake` draws the mounts again.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    use wasm_bindgen::JsCast as _;
    let apply = || {
        ui::host::install_environment(ui::host::HostEnvironment {
            accessibility: accessibility(browser_matches),
            ..ui::host::HostEnvironment::default()
        });
        changed();
    };
    apply();
    let on_change = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
        apply();
        wake();
    });
    for query in [REDUCED_MOTION, MORE_CONTRAST] {
        let Some(list) = media_query(query) else {
            continue;
        };
        if let Ok(listen) = js_sys::Reflect::get(&list, &"addEventListener".into())
            && let Some(listen) = listen.dyn_ref::<js_sys::Function>()
        {
            let _ = listen.call2(&list, &"change".into(), on_change.as_ref());
        }
    }
    // The page keeps listening as long as it lives.
    on_change.forget();
}

/// The browser's `MediaQueryList` for `query`.
#[cfg(target_arch = "wasm32")]
fn media_query(query: &str) -> Option<wasm_bindgen::JsValue> {
    use wasm_bindgen::JsCast as _;
    let match_media = js_sys::Reflect::get(&js_sys::global(), &"matchMedia".into()).ok()?;
    let match_media = match_media.dyn_ref::<js_sys::Function>()?;
    match_media.call1(&js_sys::global(), &query.into()).ok()
}

/// Whether the browser holds `query` for this page.
#[cfg(target_arch = "wasm32")]
fn browser_matches(query: &str) -> bool {
    media_query(query)
        .and_then(|list| js_sys::Reflect::get(&list, &"matches".into()).ok())
        .and_then(|matches| matches.as_bool())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_the_browser_was_asked_for_reaches_the_theme() {
        let asked = accessibility(|query| [REDUCED_MOTION, MORE_CONTRAST].contains(&query));
        assert!(asked.reduce_motion && asked.high_contrast);
        let theme = themed(ui::Theme::dark(), asked);
        assert!(theme.reduce_motion, "less motion");
        assert_ne!(
            theme.text_primary,
            ui::Theme::dark().text_primary,
            "more contrast"
        );
        let plain = themed(ui::Theme::dark(), accessibility(|_| false));
        assert!(!plain.reduce_motion);
        assert_eq!(plain.text_primary, ui::Theme::dark().text_primary);
        let before = generation();
        changed();
        assert_ne!(generation(), before, "a change draws the mounts again");
    }
}
