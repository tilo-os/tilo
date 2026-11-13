//! The page's theme, built as a native window's is: from the person's
//! settings (their theme, light or dark, contrast and motion) and from the
//! host's accessibility inputs, which on a page are what the person asked
//! of every page through their browser and system: less motion, more
//! contrast. A change to either draws every mount again in the new theme.
//!
//! A person who has chosen neither light nor dark, or chose Automatic, gets
//! the browser's, so a page is light on a light system from its first frame,
//! whichever way the mount shows it. Settings shows them Automatic chosen
//! there (the appearance section's default on a page), which on a page is
//! following the system; the themes chosen for light and for dark still show
//! in their mode.
//!
//! Every theme the page resolves goes through [`ui::Theme::configured_from`],
//! which reads the browser's answer from [`ui::theme::install_host_scheme`]:
//! what the page draws and what any surface says is in use are one reading.

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
    themed(
        ui::Theme::configured_from(&settings_store::load()),
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

/// What a page takes before anything mounts: light or dark follows the
/// browser's while the person has left it open, and its host inputs are the
/// browser's answers, now and whenever the person changes them; `wake` draws
/// the mounts again.
#[cfg(target_arch = "wasm32")]
pub(crate) fn install(wake: std::sync::Arc<dyn Fn() + Send + Sync>) {
    use wasm_bindgen::JsCast as _;
    let apply = || {
        ui::host::install_environment(ui::host::HostEnvironment {
            accessibility: accessibility(browser_matches),
            ..ui::host::HostEnvironment::default()
        });
        // What the browser answers for light or dark, so every theme resolved
        // from here on draws it, from the first frame.
        ui::theme::install_host_scheme(Some(browser_matches(DARK_SCHEME)));
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

    /// The theme a page draws for `settings` with the browser answering
    /// `scheme` — its answer for `DARK_SCHEME` as `Some(dark)` — for an
    /// appearance the person left open.
    fn from_settings(
        settings: &serde_json::Map<String, serde_json::Value>,
        scheme: Option<bool>,
    ) -> ui::Theme {
        let _scheme = ui::theme::scoped_host_scheme(scheme);
        ui::Theme::configured_from(settings)
    }

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

    /// On a fresh page Settings shows Automatic chosen, and the page draws what Automatic means
    /// there: the system's light or dark. A Dark tile ringed over a light page told the person a
    /// choice the page did not draw.
    #[test]
    fn what_settings_shows_chosen_on_a_page_is_what_the_page_draws() {
        use ui::storage::{DeviceClass, SettingsSchema as _};
        let fresh = settings_sections::prefs::AppearancePrefs::defaults_on(DeviceClass::Web);
        assert_eq!(fresh.theme_choice(), "Automatic");
        let mut chosen = settings_sections::prefs::AppearancePrefs::defaults_on(DeviceClass::Web);
        chosen.choose_theme("Automatic");
        let chosen = serde_json::json!({"appearance": {"mode": chosen.mode}});
        for dark in [false, true] {
            let untouched = from_settings(&serde_json::Map::new(), Some(dark));
            assert_eq!(untouched.document.is_dark(), dark, "it follows the system");
            let automatic = from_settings(chosen.as_object().unwrap(), Some(dark));
            assert_eq!(automatic.document.is_dark(), dark, "so does choosing it");
        }
        let dark_theme = serde_json::json!({"appearance": {"mode": "dark"}});
        assert!(
            from_settings(dark_theme.as_object().unwrap(), Some(false))
                .document
                .is_dark(),
            "Dark, chosen, is dark on a light system"
        );
        assert_eq!(
            settings_sections::prefs::AppearancePrefs::defaults_on(DeviceClass::Native)
                .theme_choice(),
            "Dark",
            "the person's own computer starts dark, and says so"
        );
    }

    #[test]
    fn the_browser_scheme_decides_only_what_the_person_left_open() {
        // What a page draws for `settings` with the browser answering `dark`, and what every
        // surface would call in use, side by side: one reading.
        let page = |settings: serde_json::Value, dark| {
            let settings = settings.as_object().cloned().unwrap_or_default();
            let mode = settings
                .get("appearance")
                .and_then(|appearance| appearance.get("mode"))
                .and_then(serde_json::Value::as_str);
            let theme = from_settings(&settings, Some(dark));
            (theme.document.is_dark(), ui::theme::drawn(mode, Some(dark)))
        };
        let drew = |settings, dark| page(settings, dark).0;
        assert!(
            drew(serde_json::json!({}), true),
            "nothing chosen, dark browser"
        );
        assert!(
            !drew(serde_json::json!({}), false),
            "nothing chosen, light browser"
        );
        assert!(
            !drew(
                serde_json::json!({"appearance": {"accent": "violet"}}),
                false
            ),
            "an accent is not a light or dark"
        );
        assert!(
            drew(serde_json::json!({"appearance": {"mode": "dark"}}), false),
            "the person's own choice stands"
        );
        assert!(
            !drew(
                serde_json::json!({"appearance": {"mode": "automatic"}}),
                false
            ),
            "Automatic follows the system"
        );
        assert!(
            !drew(
                serde_json::json!({"appearance": {"light_theme": "glass-light"}}),
                false
            ),
            "a theme chosen for one mode shows when the system is in that mode"
        );
        assert!(
            drew(
                serde_json::json!({"appearance": {"light_theme": "glass-light"}}),
                true
            ),
            "and the other mode's theme while the system is in it"
        );
        for (settings, dark) in [
            (serde_json::json!({}), true),
            (serde_json::json!({}), false),
            (serde_json::json!({"appearance": {"mode": "dark"}}), false),
            (serde_json::json!({"appearance": {"mode": "light"}}), true),
            (
                serde_json::json!({"appearance": {"mode": "automatic"}}),
                true,
            ),
            (
                serde_json::json!({"appearance": {"mode": "automatic"}}),
                false,
            ),
        ] {
            let (drawn_dark, polarity) = page(settings.clone(), dark);
            assert_eq!(
                polarity,
                ui::theme::Drawn::of(drawn_dark),
                "{settings} in a {dark} browser: in use is what is drawn"
            );
        }
        let light = from_settings(&serde_json::Map::new(), Some(false));
        assert!(!light.document.is_dark());
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
        // A page installs the browser's answer before anything mounts.
        let _light = ui::theme::scoped_host_scheme(Some(false));
        let none = serde_json::Map::new();
        let drawn = generation();
        assert!(
            !ui::Theme::configured_from(&none).document.is_dark(),
            "the first frame in a light browser is light"
        );
        // The person's system flips to dark: the browser answers the other
        // way, the page installs it, and every mount draws the scheme that is
        // now set.
        changed();
        ui::theme::install_host_scheme(Some(true));
        assert!(ui::Theme::configured_from(&none).document.is_dark());
        assert_ne!(generation(), drawn, "the mounts draw the new scheme");
    }
}
