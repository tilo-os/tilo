//! The page's theme, built as a native window's is: from the person's
//! settings (their theme, light or dark, contrast and motion) and from the
//! host's accessibility inputs, which on a page are what the person asked
//! of every page through their browser and system: less motion, more
//! contrast. A change to either draws every mount again in the new theme.
//!
//! A person who has chosen neither light nor dark gets the browser's, so a
//! page is light on a light system from its first frame, whichever way the
//! mount shows it.

use std::cell::Cell;

/// The media query the browser answers for a person who asked for less
/// motion.
pub(crate) const REDUCED_MOTION: &str = "(prefers-reduced-motion: reduce)";
/// The media query the browser answers for a person who asked for more
/// contrast.
pub(crate) const MORE_CONTRAST: &str = "(prefers-contrast: more)";
/// The media query the browser answers for a person whose browser and
/// system are dark.
#[cfg(target_arch = "wasm32")]
pub(crate) const DARK_SCHEME: &str = "(prefers-color-scheme: dark)";

thread_local! {
    /// Bumped whenever what the theme is built from changes; a mount that
    /// drew at another generation takes the theme again.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Whether light or dark follows the browser's colour scheme while the
    /// person has not chosen one ([`follow_browser_scheme`]).
    static FOLLOW_SCHEME: Cell<bool> = const { Cell::new(false) };
}

/// Have light or dark follow the browser's colour scheme (the person's
/// system setting, as every page sees it) for as long as the person has
/// not chosen one in their settings. A page does, whichever way it shows
/// its mounts: [`install`] asks for it before the first frame.
pub(crate) fn follow_browser_scheme() {
    if !FOLLOW_SCHEME.with(|follow| follow.replace(true)) {
        changed();
    }
}

/// `settings` with light or dark from the browser (`dark`) when they name
/// neither: no mode and no theme for one.
pub(crate) fn with_browser_scheme(
    mut settings: serde_json::Map<String, serde_json::Value>,
    dark: bool,
) -> serde_json::Map<String, serde_json::Value> {
    let appearance = settings
        .entry("appearance")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if let Some(appearance) = appearance.as_object_mut()
        && !["mode", "dark_theme", "light_theme"]
            .iter()
            .any(|key| appearance.contains_key(*key))
    {
        appearance.insert(
            "mode".into(),
            serde_json::Value::from(if dark { "dark" } else { "light" }),
        );
    }
    settings
}

/// The theme `settings` name, with light or dark from the browser's
/// `scheme` — its answer for `DARK_SCHEME` as `Some(dark)` — when the
/// person has chosen neither.
pub(crate) fn from_settings(
    settings: &serde_json::Map<String, serde_json::Value>,
    scheme: Option<bool>,
) -> ui::Theme {
    match scheme {
        Some(dark) => ui::Theme::configured_from(&with_browser_scheme(settings.clone(), dark)),
        None => ui::Theme::configured_from(settings),
    }
}

/// The theme as custom properties for the page (`--tilo-*`), with the
/// `color-scheme` the browser draws its own parts in: what a page shown as
/// HTML styles its native fields, scrollbars and selection with.
pub(crate) fn css_variables(theme: &ui::Theme) -> Vec<(&'static str, String)> {
    let colors = &theme.tokens.colors;
    let css = |color: ui::peniko::Color| {
        let rgba = color.to_rgba8();
        format!(
            "rgba({},{},{},{:.3})",
            rgba.r,
            rgba.g,
            rgba.b,
            f32::from(rgba.a) / 255.0
        )
    };
    vec![
        (
            "color-scheme",
            if theme.document.is_dark() {
                "dark"
            } else {
                "light"
            }
            .to_owned(),
        ),
        ("--tilo-ground", css(colors.surfaces.content)),
        ("--tilo-sidebar", css(colors.surfaces.sidebar)),
        ("--tilo-raised", css(colors.surfaces.raised)),
        ("--tilo-overlay", css(colors.surfaces.overlay)),
        ("--tilo-text", css(theme.text_primary)),
        ("--tilo-text-dim", css(theme.text_dim)),
        ("--tilo-text-tertiary", css(theme.text_tertiary)),
        ("--tilo-border", css(theme.border_standard)),
        ("--tilo-focus", css(theme.border_focus)),
        ("--tilo-accent", css(colors.intent.accent)),
        ("--tilo-on-accent", css(colors.intent.on_accent)),
        ("--tilo-radius", format!("{:.1}px", theme.radius_standard)),
    ]
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
    let scheme = FOLLOW_SCHEME
        .with(Cell::get)
        .then(|| browser_matches(DARK_SCHEME));
    themed(
        from_settings(&settings_store::load(), scheme),
        ui::host::environment().accessibility,
    )
}

/// Which change of what the theme is built from the page is at. A theme's
/// surface shader is checked once its code is in, so its arrival is one.
pub(crate) fn generation() -> u64 {
    GENERATION
        .with(Cell::get)
        .wrapping_add(ui::part_paint::SURFACE_SHADER.generation())
}

/// What the theme is built from changed: every mount takes it again on its
/// next frame.
pub(crate) fn changed() {
    GENERATION.with(|generation| generation.set(generation.get().wrapping_add(1)));
}

/// What a page takes before anything mounts: its theme follows the
/// browser's light or dark while the person has chosen neither, and its
/// host inputs are the browser's answers, now and whenever the person
/// changes them; `wake` draws the mounts again.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    use wasm_bindgen::JsCast as _;
    follow_browser_scheme();
    let apply = || {
        ui::host::install_environment(ui::host::HostEnvironment {
            accessibility: accessibility(browser_matches),
            ..ui::host::HostEnvironment::default()
        });
        // Less motion, more contrast and, on a page that follows it, light
        // or dark: the mounts take the theme again on their next frame.
        changed();
    };
    apply();
    let on_change = wasm_bindgen::closure::Closure::<dyn Fn()>::new(move || {
        apply();
        wake();
    });
    for query in [REDUCED_MOTION, MORE_CONTRAST, DARK_SCHEME] {
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
pub(crate) fn media_query(query: &str) -> Option<wasm_bindgen::JsValue> {
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

    #[test]
    fn the_browser_scheme_decides_only_what_the_person_left_open() {
        let mode = |settings: serde_json::Value, dark| {
            with_browser_scheme(settings.as_object().cloned().unwrap_or_default(), dark)["appearance"]["mode"].clone()
        };
        assert_eq!(mode(serde_json::json!({}), true), "dark");
        assert_eq!(mode(serde_json::json!({}), false), "light");
        assert_eq!(
            mode(
                serde_json::json!({"appearance": {"accent": "violet"}}),
                false
            ),
            "light"
        );
        assert_eq!(
            mode(serde_json::json!({"appearance": {"mode": "dark"}}), false),
            "dark",
            "the person's own choice stands"
        );
        assert_eq!(
            mode(
                serde_json::json!({"appearance": {"light_theme": "nord"}}),
                true
            ),
            serde_json::Value::Null,
            "a theme chosen for one mode is a choice too"
        );
        let light = ui::Theme::configured_from(&with_browser_scheme(serde_json::Map::new(), false));
        let dark = ui::Theme::configured_from(&with_browser_scheme(serde_json::Map::new(), true));
        assert!(!light.document.is_dark() && dark.document.is_dark());
        let variables = css_variables(&light);
        assert!(variables.contains(&("color-scheme", "light".to_owned())));
        assert!(
            variables
                .iter()
                .any(|(name, value)| *name == "--tilo-text" && value.starts_with("rgba("))
        );
    }

    #[test]
    fn a_first_visit_draws_the_browser_scheme() {
        // Nothing chosen: the browser's light or dark is the page's.
        let none = serde_json::Map::new();
        assert!(
            !from_settings(&none, Some(false)).document.is_dark(),
            "settings with no appearance in a light browser are light"
        );
        assert!(
            from_settings(&none, Some(true)).document.is_dark(),
            "and dark in a dark one"
        );
        // The person's own choice is what the page draws, whatever the
        // browser answers.
        let chosen = serde_json::json!({"appearance": {"mode": "dark"}});
        assert!(
            from_settings(
                &chosen.as_object().cloned().unwrap_or_default(),
                Some(false)
            )
            .document
            .is_dark(),
            "an explicit dark wins over a light browser"
        );
        // A caller that does not follow (a native window) keeps the
        // settings' own default.
        assert!(from_settings(&none, None).document.is_dark());
    }

    #[test]
    fn a_scheme_change_while_following_redraws() {
        // A page follows from `install`, before anything mounts.
        follow_browser_scheme();
        let none = serde_json::Map::new();
        let drawn = generation();
        assert!(
            !from_settings(&none, Some(false)).document.is_dark(),
            "the first frame in a light browser is light"
        );
        // The person's system flips to dark: the browser answers the other
        // way, and the listener the page installed takes the theme again,
        // so every mount draws in the scheme that is now set.
        changed();
        assert!(from_settings(&none, Some(true)).document.is_dark());
        assert_ne!(generation(), drawn, "the mounts draw the new scheme");
        // Following once is enough: a page that already does takes no
        // further generation for it.
        let followed = generation();
        follow_browser_scheme();
        assert_eq!(generation(), followed);
    }
}
