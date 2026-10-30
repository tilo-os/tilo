//! The person's accessibility settings, as the compositor takes them.
//!
//! The compositor knows nothing of the settings store: it takes an
//! [`A11y`] and applies it live. This is the one place the stored section
//! becomes that value, so every part of the session that applies the
//! settings — on start and on every change — reads them the same way.

#![allow(
    dead_code,
    reason = "the session's settings loader applies these; nothing else reads them"
)]

use std::time::Duration;

use settings_store::magnifier::MagnifierView as StoredView;
use tilo_accessibility_settings::{
    AccessibilitySettings, ColorFilter as StoredFilter, OnScreenKeyboard,
    SwitchKeyPair as StoredPair,
};
use tilo_compositor::{
    A11y, Assists, ColorFilter, Compositor, CursorA11y, Error, Magnifier, MagnifierView,
    SwitchConfig, SwitchPair, TextEntry,
};

/// How long a key must be held for slow keys to type it: long enough that a
/// brush past a key is not a keystroke, short enough to type with.
pub const SLOW_KEYS_DELAY: Duration = Duration::from_millis(300);

/// The compositor's accessibility settings for `settings`.
#[must_use]
pub fn a11y(settings: &AccessibilitySettings) -> A11y {
    let bounce = settings.bounce_window();
    A11y {
        assists: Assists {
            sticky_keys: settings.sticky_keys,
            slow_keys: settings.slow_keys.then_some(SLOW_KEYS_DELAY),
            bounce_keys: (!bounce.is_zero()).then_some(bounce),
            bounce_alert: settings.bounce_keys_alert,
            mouse_keys: settings.mouse_keys,
            switch: settings.switch_control.then(|| SwitchConfig {
                pair: switch_pair(settings.switch_key_pair),
                period: settings.switch_scan(),
            }),
        },
        cursor: CursorA11y {
            size: settings.effective_cursor_size(),
            crosshair: settings.cursor_crosshair,
        },
        color_filter: color_filter(settings.color_filter),
        magnifier: magnifier(settings),
        visual_alerts: settings.visual_alerts,
    }
}

/// Apply the person's accessibility settings to `compositor`, now.
///
/// # Errors
///
/// When the compositor cannot flush the answers to clients.
pub fn apply(compositor: &mut Compositor, settings: &AccessibilitySettings) -> Result<(), Error> {
    compositor.set_a11y(a11y(settings))
}

/// Whether the on-screen keyboard belongs on screen: the person's choice of
/// when it runs, and what the compositor sees of a text field waiting for
/// typing and of the way they are typing.
#[must_use]
pub fn keyboard_visible(preference: OnScreenKeyboard, entry: TextEntry) -> bool {
    match preference {
        OnScreenKeyboard::Always => true,
        OnScreenKeyboard::Never => false,
        // The keyboard arrives with a text field when the person types on the
        // glass, and stays out of the way of the one they are holding.
        OnScreenKeyboard::Auto => entry.wants_typing && entry.on_the_glass,
    }
}

/// The stored colour filter as the compositor's.
fn color_filter(filter: StoredFilter) -> ColorFilter {
    match filter {
        StoredFilter::None => ColorFilter::Off,
        StoredFilter::Grayscale => ColorFilter::Grayscale,
        StoredFilter::Inverted => ColorFilter::Inverted,
        StoredFilter::GrayscaleInverted => ColorFilter::GrayscaleInverted,
    }
}

/// The stored switch pair as the compositor's.
fn switch_pair(pair: StoredPair) -> SwitchPair {
    match pair {
        StoredPair::F6F7 => SwitchPair::F6F7,
        StoredPair::F9F10 => SwitchPair::F9F10,
        StoredPair::Digits12 => SwitchPair::Digits12,
    }
}

/// The magnifier's zoom and view.
fn magnifier(settings: &AccessibilitySettings) -> Magnifier {
    let stored = settings.magnifier();
    Magnifier {
        zoom: stored.zoom,
        view: match stored.view {
            StoredView::Lens => MagnifierView::Lens,
            StoredView::FullScreen => MagnifierView::FullScreen,
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;
    use tilo_accessibility_settings::{ColorFilter as StoredFilter, SwitchKeyPair as StoredPair};

    use super::*;

    /// Settings read from `section`, as the store would hand them over.
    fn settings(section: &[(&str, serde_json::Value)]) -> AccessibilitySettings {
        let mut map = Map::new();
        for (name, value) in section {
            map.insert((*name).to_owned(), value.clone());
        }
        AccessibilitySettings::from_section(&map)
    }

    #[test]
    fn applying_reaches_the_compositor() {
        let mut compositor =
            Compositor::headless(Box::new(tilo_compositor::StackPolicy)).expect("compositor");
        apply(
            &mut compositor,
            &settings(&[
                ("sticky_keys", true.into()),
                ("color_filter", "inverted".into()),
                ("cursor_crosshair", true.into()),
            ]),
        )
        .expect("apply");
        assert!(compositor.a11y().assists.sticky_keys);
        assert_eq!(compositor.a11y().color_filter, ColorFilter::Inverted);
        assert!(compositor.a11y().cursor.crosshair);

        // And back off again, as the person turning them off would.
        apply(&mut compositor, &settings(&[])).expect("apply");
        assert!(compositor.a11y().assists.is_off());
        assert_eq!(compositor.a11y().color_filter, ColorFilter::Off);
        assert!(!compositor.a11y().cursor.crosshair);
    }

    #[test]
    fn settings_off_leave_every_assist_off() {
        let a11y = a11y(&settings(&[]));
        assert!(a11y.assists.is_off());
        assert!(!a11y.visual_alerts);
        assert_eq!(a11y.cursor.size, 24);
        assert!(!a11y.cursor.crosshair);
        assert_eq!(a11y.color_filter, ColorFilter::Off);
        assert_eq!(a11y.magnifier.zoom, 1, "the magnifier starts off");
        assert_eq!(a11y.magnifier.view, MagnifierView::Lens);
    }

    #[test]
    fn every_assist_reaches_the_compositor() {
        let a11y = a11y(&settings(&[
            ("sticky_keys", true.into()),
            ("slow_keys", true.into()),
            ("bounce_keys", true.into()),
            ("bounce_keys_alert", true.into()),
            ("mouse_keys", true.into()),
            ("switch_control", true.into()),
            ("switch_key_pair", "f9_f10".into()),
            ("switch_scan_ms", 1200.into()),
            ("visual_alerts", true.into()),
        ]));
        assert!(a11y.assists.sticky_keys);
        assert_eq!(a11y.assists.slow_keys, Some(SLOW_KEYS_DELAY));
        assert_eq!(
            a11y.assists.bounce_keys,
            Some(Duration::from_millis(u64::from(
                tilo_accessibility_settings::BOUNCE_KEYS_DEFAULT_DELAY_MS
            )))
        );
        assert!(a11y.assists.bounce_alert);
        assert!(a11y.assists.mouse_keys);
        let switch = a11y.assists.switch.expect("switch scanning is on");
        assert_eq!(switch.pair, SwitchPair::F9F10);
        assert_eq!(switch.period, Some(Duration::from_millis(1200)));
        assert!(a11y.visual_alerts);
    }

    #[test]
    fn no_scan_period_means_the_switches_step_by_hand() {
        let a11y = a11y(&settings(&[
            ("switch_control", true.into()),
            ("switch_scan_ms", 0.into()),
        ]));
        assert_eq!(a11y.assists.switch.expect("on").period, None);
    }

    #[test]
    fn a_larger_pointer_is_only_large_with_the_setting_on() {
        let plain = a11y(&settings(&[("cursor_size", 48.into())]));
        assert_eq!(plain.cursor.size, 24);
        let large = a11y(&settings(&[
            ("large_cursor", true.into()),
            ("cursor_size", 48.into()),
        ]));
        assert_eq!(large.cursor.size, 48);
    }

    #[test]
    fn a_crosshair_goes_with_its_setting() {
        let a11y = a11y(&settings(&[("cursor_crosshair", true.into())]));
        assert!(a11y.cursor.crosshair);
    }

    #[test]
    fn every_colour_filter_maps_to_the_compositor_s_own() {
        for (stored, compositor) in [
            (StoredFilter::None, ColorFilter::Off),
            (StoredFilter::Grayscale, ColorFilter::Grayscale),
            (StoredFilter::Inverted, ColorFilter::Inverted),
            (
                StoredFilter::GrayscaleInverted,
                ColorFilter::GrayscaleInverted,
            ),
        ] {
            assert_eq!(color_filter(stored), compositor, "{stored:?}");
        }
    }

    #[test]
    fn every_switch_pair_maps_to_the_compositor_s_own() {
        for (stored, compositor) in [
            (StoredPair::F6F7, SwitchPair::F6F7),
            (StoredPair::F9F10, SwitchPair::F9F10),
            (StoredPair::Digits12, SwitchPair::Digits12),
        ] {
            assert_eq!(switch_pair(stored), compositor, "{stored:?}");
        }
    }

    #[test]
    fn the_magnifier_carries_its_zoom_and_view() {
        let a11y = a11y(&settings(&[
            ("magnifier_zoom", 3.into()),
            ("magnifier_view", "full_screen".into()),
        ]));
        assert_eq!(a11y.magnifier.zoom, 3);
        assert_eq!(a11y.magnifier.view, MagnifierView::FullScreen);
        assert!(a11y.magnifier.factor() > 2.0);
    }

    /// A text field waiting for typing, typed on the glass or not.
    fn entry(wants_typing: bool, on_the_glass: bool) -> TextEntry {
        TextEntry {
            wants_typing,
            on_the_glass,
        }
    }

    #[test]
    fn the_on_screen_keyboard_follows_its_setting_and_the_glass() {
        // Automatic: with a text field and a touch on the screen.
        assert!(keyboard_visible(OnScreenKeyboard::Auto, entry(true, true)));
        // Not while they type on the keyboard they hold.
        assert!(!keyboard_visible(
            OnScreenKeyboard::Auto,
            entry(true, false)
        ));
        // Not over a desktop with no field waiting for anything.
        assert!(!keyboard_visible(
            OnScreenKeyboard::Auto,
            entry(false, true)
        ));
        // Always: up even before a field is reached.
        assert!(keyboard_visible(
            OnScreenKeyboard::Always,
            entry(false, false)
        ));
        // Never: not even for a touch in a field.
        assert!(!keyboard_visible(
            OnScreenKeyboard::Never,
            entry(true, true)
        ));
    }

    #[test]
    fn the_stored_keyboard_preference_is_the_one_that_applies() {
        for (stored, expected) in [
            ("never", OnScreenKeyboard::Never),
            ("always", OnScreenKeyboard::Always),
            ("automatic", OnScreenKeyboard::Auto),
        ] {
            assert_eq!(
                settings(&[("on_screen_keyboard", stored.into())]).on_screen_keyboard,
                expected,
                "{stored}"
            );
        }
    }
}
