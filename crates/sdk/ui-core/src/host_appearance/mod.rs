//! The appearance a standalone app takes from the operating system it runs
//! on. Inside Tilo OS the shell supplies appearance through settings and
//! nothing here runs; outside it, light/dark, the accent and the contrast
//! and motion preferences follow the host, so an SDK app looks at home
//! without choosing a colour of its own.
//!
//! Light/dark comes from winit (`Window::theme`, `ThemeChanged`) where it
//! reports one. macOS adds its accent and accessibility display options;
//! Linux reads everything from the XDG Settings portal, whose scheme wins
//! because winit reports none there.

use crate::theme::Theme;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod portal;

#[cfg(target_os = "macos")]
use macos::Platform;
#[cfg(target_os = "linux")]
use portal::Platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scheme {
    Light,
    Dark,
}

impl From<winit::window::Theme> for Scheme {
    fn from(theme: winit::window::Theme) -> Self {
        match theme {
            winit::window::Theme::Light => Self::Light,
            winit::window::Theme::Dark => Self::Dark,
        }
    }
}

/// One reading of the host's appearance preferences. `None` means the host
/// states no preference, which keeps Tilo's own default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HostAppearance {
    pub scheme: Option<Scheme>,
    pub accent: Option<[u8; 3]>,
    pub high_contrast: bool,
    pub reduce_motion: bool,
}

impl HostAppearance {
    /// The colour theme only; the App layers contrast and motion on top.
    pub fn theme(&self) -> Theme {
        let base = match self.scheme {
            Some(Scheme::Light) => Theme::light(),
            Some(Scheme::Dark) | None => Theme::dark(),
        };
        match self.accent {
            Some(accent) => base.with_accent(accent),
            None => base,
        }
    }
}

/// Whether the host, not the Tilo shell, owns this process's appearance.
/// `desktop` is `XDG_CURRENT_DESKTOP`, a colon-separated list.
pub(crate) fn host_owns_appearance(desktop: Option<&str>) -> bool {
    !desktop.is_some_and(|desktop| {
        desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("tilo"))
    })
}

/// The native runner's view of the host appearance, reporting each change
/// once.
pub(crate) struct Source {
    window_scheme: Option<Scheme>,
    platform: Platform,
    /// Accessibility the launcher requested; the host can add to it, never
    /// take it away.
    requested: crate::host::AccessibilityInputs,
    applied: Option<HostAppearance>,
}

impl Source {
    /// `None` inside Tilo OS. Start it under the app's wake scope: a platform
    /// that reports changes itself wakes the event loop through it.
    pub fn start() -> Option<Self> {
        let environment = crate::host::environment();
        host_owns_appearance(environment.desktop.as_deref()).then(|| Self {
            window_scheme: None,
            platform: Platform::start(),
            requested: environment.accessibility,
            applied: None,
        })
    }

    pub fn note_window_theme(&mut self, theme: Option<winit::window::Theme>) {
        self.window_scheme = theme.map(Scheme::from);
    }

    /// Re-read what the platform reports only when asked.
    pub fn refresh(&mut self) {
        self.platform.refresh();
    }

    fn current(&self) -> HostAppearance {
        let platform = self.platform.current();
        HostAppearance {
            scheme: platform.scheme.or(self.window_scheme),
            high_contrast: platform.high_contrast || self.requested.high_contrast,
            reduce_motion: platform.reduce_motion || self.requested.reduce_motion,
            ..platform
        }
    }

    /// The appearance to apply when it changed since the last call, or
    /// always with `force` — after something else re-themed the app.
    pub fn take_change(&mut self, force: bool) -> Option<HostAppearance> {
        let current = self.current();
        if !force && self.applied == Some(current) {
            return None;
        }
        self.applied = Some(current);
        Some(current)
    }
}

/// A host whose only appearance signal is winit's light/dark.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
#[derive(Default)]
struct Platform;

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl Platform {
    fn start() -> Self {
        Self
    }

    fn refresh(&mut self) {}

    #[allow(
        clippy::unused_self,
        reason = "the same shape as the platforms that do read something"
    )]
    fn current(&self) -> HostAppearance {
        HostAppearance::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::colors::ACCENT_PRIMARY_IDX;
    use vello::peniko::Color;

    #[test]
    fn scheme_picks_the_matching_base_and_no_preference_is_dark() {
        let theme = |scheme| {
            HostAppearance {
                scheme,
                ..HostAppearance::default()
            }
            .theme()
        };
        assert_eq!(theme(Some(Scheme::Light)).name, Theme::light().name);
        assert_eq!(theme(Some(Scheme::Light)).bg_app, Theme::light().bg_app);
        assert_eq!(theme(Some(Scheme::Dark)).bg_app, Theme::dark().bg_app);
        assert_eq!(theme(None).bg_app, Theme::dark().bg_app);
        assert_eq!(
            theme(None).accent_primary,
            Theme::dark().accent_primary,
            "no host accent keeps Tilo's own"
        );
    }

    #[test]
    fn host_accent_recolours_every_accent_token() {
        let theme = HostAppearance {
            scheme: Some(Scheme::Light),
            accent: Some([0xE0, 0x40, 0x80]),
            ..HostAppearance::default()
        }
        .theme();
        let accent = Color::from_rgb8(0xE0, 0x40, 0x80);
        assert_eq!(theme.accent_primary, accent);
        assert_eq!(theme.border_focus, accent);
        assert_eq!(theme.tokens.colors.accent[ACCENT_PRIMARY_IDX], accent);
        assert_eq!(theme.tokens.colors.border_focus, accent);
        assert_ne!(theme.accent_hover, accent, "hover stays a distinct step");
        assert_ne!(
            theme.tokens.terminal_palette.ansi,
            Theme::light().tokens.terminal_palette.ansi,
            "derived palettes follow the new accent"
        );
        assert_eq!(
            theme.bg_app,
            Theme::light().bg_app,
            "surfaces are untouched"
        );
    }

    #[test]
    fn only_a_tilo_session_keeps_the_shell_in_charge() {
        assert!(!host_owns_appearance(Some("tilo")));
        assert!(!host_owns_appearance(Some("GNOME:Tilo")));
        assert!(host_owns_appearance(Some("GNOME")));
        assert!(host_owns_appearance(None));
    }

    /// A source with no platform reading: winit and the launcher only.
    fn source(requested: crate::host::AccessibilityInputs) -> Source {
        Source {
            window_scheme: None,
            platform: Platform::default(),
            requested,
            applied: None,
        }
    }

    #[test]
    fn each_change_is_reported_once_unless_forced() {
        let mut source = source(crate::host::AccessibilityInputs::default());
        assert_eq!(
            source.take_change(false),
            Some(HostAppearance::default()),
            "the first reading applies"
        );
        assert_eq!(source.take_change(false), None);
        assert!(source.take_change(true).is_some(), "force re-applies");
        source.note_window_theme(Some(winit::window::Theme::Light));
        let change = source.take_change(false).expect("scheme change");
        assert_eq!(change.scheme, Some(Scheme::Light));
        assert_eq!(source.take_change(false), None);
    }

    #[test]
    fn launcher_accessibility_requests_survive_the_host() {
        let mut source = source(crate::host::AccessibilityInputs {
            high_contrast: true,
            reduce_motion: true,
            ..crate::host::AccessibilityInputs::default()
        });
        let applied = source.take_change(false).expect("first reading");
        assert!(applied.high_contrast);
        assert!(applied.reduce_motion);
    }
}
