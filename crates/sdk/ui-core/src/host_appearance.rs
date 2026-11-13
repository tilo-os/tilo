//! The appearance a standalone app takes from the operating system it runs on. Inside Tilo OS the shell
//! supplies appearance through settings and nothing here runs; outside it, light/dark, the contrast and
//! motion preferences follow the host, and the presence hue nearest the host's accent becomes the
//! person's own colour (focus ring, caret, selection), so an SDK app looks at home without choosing a
//! colour of its own.

use crate::theme::Theme;

pub(crate) use tilo_appearance::{Appearance as HostAppearance, Scheme};

/// The colour theme only; the App layers contrast and motion on top.
pub(crate) fn theme(host: HostAppearance) -> Theme {
    let base = match host.scheme {
        Some(Scheme::Light) => Theme::light(),
        Some(Scheme::Dark) | None => Theme::dark(),
    };
    let accent = host.accent.and_then(|[r, g, b]| {
        crate::tokens::nearest_presence_hue(vello::peniko::Color::from_rgb8(r, g, b))
    });
    match accent {
        Some(hue) => base.with_your_colour(hue),
        None => base,
    }
}

fn scheme(theme: winit::window::Theme) -> Scheme {
    match theme {
        winit::window::Theme::Light => Scheme::Light,
        winit::window::Theme::Dark => Scheme::Dark,
    }
}

/// The shell supplies this process's settings through its app-scoped collaboration channel, which owns
/// the theme the shell chrome already paints from. Sandboxes clear `XDG_CURRENT_DESKTOP` and may not
/// expose the host Settings portal; its unset scheme must not paint the dark fallback over the shell's
/// configured theme.
pub(crate) fn session_document_owns_appearance() -> bool {
    settings_store::installed_source().is_some()
}

/// Whether the host, not the Tilo shell, owns this process's appearance.
pub(crate) fn host_owns_appearance(desktop: Option<&str>) -> bool {
    !desktop.is_some_and(|desktop| {
        desktop
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("tilo"))
    })
}

/// The native runner's view of the host appearance, reporting each change once.
pub(crate) struct Source {
    window_scheme: Option<Scheme>,
    system: tilo_appearance::System,
    /// Accessibility the launcher requested; the host can add to it, never take it away.
    requested: crate::host::AccessibilityInputs,
    applied: Option<HostAppearance>,
}

impl Source {
    /// `None` when the shell's settings source or session document owns appearance, and when the
    /// desktop is already Tilo.
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

    /// The appearance to apply when it changed since the last call, or always with `force` — after
    /// something else re-themed the app.
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
    use crate::color::Oklch;
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
        assert_eq!(
            theme(Some(Scheme::Light)).tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content
        );
        assert_eq!(
            theme(Some(Scheme::Dark)).tokens.colors.surfaces.content,
            Theme::dark().tokens.colors.surfaces.content
        );
        assert_eq!(
            theme(None).tokens.colors.surfaces.content,
            Theme::dark().tokens.colors.surfaces.content
        );
        assert_eq!(
            theme(None).border_focus,
            Theme::dark().border_focus,
            "no host accent keeps the default colour"
        );
    }

    #[test]
    fn the_host_accent_becomes_your_colour_at_presence_lightness() {
        let theme = theme(HostAppearance {
            scheme: Some(Scheme::Light),
            accent: Some([0xE0, 0x40, 0x80]),
            ..HostAppearance::default()
        });
        let accent = Color::from_rgb8(0xE0, 0x40, 0x80);
        let you = theme.tokens.colors.intent.you;
        assert_eq!(theme.border_focus, theme.tokens.colors.intent.accent_ink);
        let ring = Oklch::from_color(theme.border_focus);
        let ring_d = (ring.h - Oklch::from_color(you).h).rem_euclid(360.0);
        assert!(
            ring_d.min(360.0 - ring_d) <= 15.0,
            "the ring is your colour's hue"
        );
        let (a, y) = (Oklch::from_color(accent), Oklch::from_color(you));
        let d = (a.h - y.h).rem_euclid(360.0);
        assert!(
            d.min(360.0 - d) <= 15.0,
            "the presence hue nearest the host's is taken"
        );
        assert_eq!(
            Some(theme.tokens.colors.you),
            crate::tokens::nearest_presence_hue(accent)
        );
        assert!(
            (y.l - theme.tokens.colors.presence.lightness).abs() < 0.01,
            "at the presence lightness, not the host's"
        );
        assert_eq!(
            theme.tokens.colors.surfaces.content,
            Theme::light().tokens.colors.surfaces.content,
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
    fn the_shells_settings_own_appearance_and_their_absence_does_not() {
        let _profile = settings_store::scoped_source(None);
        let mut env = tilo_test_env::ScopedEnv::new();
        assert!(!session_document_owns_appearance());

        // A native app's sandbox has no session file or desktop name. Its
        // app-scoped shell settings still own appearance, including a custom
        // theme the host portal cannot describe.
        env.remove("XDG_CURRENT_DESKTOP");
        let appearance = serde_json::json!({"appearance": {"mode": "light"}});
        let _shell = settings_store::scoped_source(Some(std::sync::Arc::new(
            settings_store::HostDocument::new(appearance.as_object().unwrap().clone(), |_| Ok(())),
        )));
        assert_eq!(Theme::configured().name, Theme::light().name);
        assert!(
            Source::start().is_none(),
            "the native runner must not start a host fallback over the shell's settings"
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

    /// Tests share one process: a settings document one installs process-wide is read by every test
    /// running beside it, which is how one test's light theme reached another's high-contrast toggle.
    /// The toolkit's tests scope theirs to their thread (`settings_store::scoped_source`); the toolkit
    /// itself never installs one, its host does.
    #[test]
    fn no_toolkit_code_installs_a_process_wide_settings_document() {
        let needle = concat!("settings_store::", "install_source(");
        let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        let mut found = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && std::fs::read_to_string(&path).is_ok_and(|text| text.contains(needle))
                {
                    found.push(path.display().to_string());
                }
            }
        }
        assert!(found.is_empty(), "{found:?}");
    }
}
