//! The appearance a standalone app takes from the operating system it runs
//! on. Inside Tilo OS the shell supplies appearance through settings and
//! nothing here runs; outside it, light/dark, the accent and the contrast
//! and motion preferences follow the host, so an SDK app looks at home
//! without choosing a colour of its own.
//!
//! Light/dark comes from winit (`Window::theme`, `ThemeChanged`) where it
//! reports one; `tilo_appearance` adds what the desktop itself reports, and
//! its scheme wins because winit reports none where the desktop has one.

use crate::theme::Theme;

pub(crate) use tilo_appearance::{Appearance as HostAppearance, Scheme};

/// The colour theme only; the App layers contrast and motion on top.
pub(crate) fn theme(host: HostAppearance) -> Theme {
    let base = match host.scheme {
        Some(Scheme::Light) => Theme::light(),
        Some(Scheme::Dark) | None => Theme::dark(),
    };
    match host.accent {
        Some(accent) => base.with_accent(accent),
        None => base,
    }
}

fn scheme(theme: winit::window::Theme) -> Scheme {
    match theme {
        winit::window::Theme::Light => Scheme::Light,
        winit::window::Theme::Dark => Scheme::Dark,
    }
}

/// The launcher named the session `settings.json` for this process.
/// That document is the theme the shell chrome already paints from. An
/// unset host portal must not replace it: the Settings sandbox clears
/// `XDG_CURRENT_DESKTOP` and is not granted the Settings portal, so the
/// host reading stays "no scheme" and would paint the dark default back
/// over every session-file change.
pub(crate) fn session_document_owns_appearance() -> bool {
    crate::host::environment().session_settings.is_some()
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
    system: tilo_appearance::System,
    /// Accessibility the launcher requested; the host can add to it, never
    /// take it away.
    requested: crate::host::AccessibilityInputs,
    applied: Option<HostAppearance>,
}

impl Source {
    /// `None` when the session settings document owns appearance, and when
    /// the desktop is already Tilo. Otherwise the host portal owns it.
    /// Start it under the app's wake scope: a platform that reports changes
    /// itself wakes the event loop through it.
    pub fn start() -> Option<Self> {
        if session_document_owns_appearance() {
            return None;
        }
        let environment = crate::host::environment();
        host_owns_appearance(environment.desktop.as_deref()).then(|| Self {
            window_scheme: None,
            system: tilo_appearance::System::follow({
                let wake = crate::wake::WakeHandle::new();
                move || wake.wake()
            }),
            requested: environment.accessibility,
            applied: None,
        })
    }

    pub fn note_window_theme(&mut self, theme: Option<winit::window::Theme>) {
        self.window_scheme = theme.map(scheme);
    }

    /// Re-read what the platform reports only when asked.
    pub fn refresh(&mut self) {
        self.system.refresh();
    }

    fn current(&self) -> HostAppearance {
        let desktop = self.system.current();
        HostAppearance {
            scheme: desktop.scheme.or(self.window_scheme),
            high_contrast: desktop.high_contrast || self.requested.high_contrast,
            reduce_motion: desktop.reduce_motion || self.requested.reduce_motion,
            ..desktop
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens::colors::ACCENT_PRIMARY_IDX;
    use vello::peniko::Color;

    #[test]
    fn scheme_picks_the_matching_base_and_no_preference_is_dark() {
        let theme = |scheme| {
            theme(HostAppearance {
                scheme,
                ..HostAppearance::default()
            })
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
        let theme = theme(HostAppearance {
            scheme: Some(Scheme::Light),
            accent: Some([0xE0, 0x40, 0x80]),
            ..HostAppearance::default()
        });
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

    #[test]
    #[serial_test::serial(process_env)]
    fn a_named_session_document_owns_appearance_and_an_absent_one_does_not() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(settings_store::SESSION_SETTINGS_ENV);
        assert!(!session_document_owns_appearance());
        env.set(
            settings_store::SESSION_SETTINGS_ENV,
            "/run/tilo/app/.config/tilo/settings.json",
        );
        assert!(session_document_owns_appearance());
        env.set(settings_store::SESSION_SETTINGS_ENV, "");
        assert!(
            !session_document_owns_appearance(),
            "an empty path is not a session document"
        );
    }

    /// A source with no desktop reading: winit and the launcher only.
    fn source(requested: crate::host::AccessibilityInputs) -> Source {
        Source {
            window_scheme: None,
            system: tilo_appearance::System::default(),
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
