//! Who draws the entry points to History and Search on this host.

use crate::platform::{Platform, PlatformConventions};

/// Who presents the entry points to History and Search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostChrome {
    /// The Tilo shell owns History (Super+H) and Search; the app shows none.
    TiloShell,
    /// The host's own chrome: the macOS menu bar and title bar, the Windows
    /// menu bar and Jump List, or the header bar a Linux app draws where the
    /// desktop leaves decorations to it.
    Native,
    /// No native backend yet: buttons inside the window.
    InWindow,
}

impl HostChrome {
    /// `desktop` is `XDG_CURRENT_DESKTOP`, a colon-separated list.
    pub(crate) fn detect(conventions: PlatformConventions, desktop: Option<&str>) -> Self {
        if desktop.is_some_and(|desktop| {
            desktop
                .split(':')
                .any(|name| name.eq_ignore_ascii_case("tilo"))
        }) {
            return Self::TiloShell;
        }
        match conventions.platform() {
            Platform::MacOs if cfg!(all(target_os = "macos", feature = "native")) => Self::Native,
            Platform::Windows if cfg!(all(windows, feature = "native")) => Self::Native,
            _ => Self::InWindow,
        }
    }

    pub(crate) fn current(conventions: PlatformConventions) -> Self {
        Self::detect(conventions, crate::host::environment().desktop.as_deref())
    }

    pub(crate) fn draws_in_window(self) -> bool {
        self == Self::InWindow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hosts_without_their_own_chrome_draw_in_window() {
        let linux = PlatformConventions::new(Platform::Linux);
        let windows = PlatformConventions::new(Platform::Windows);
        assert_eq!(
            HostChrome::detect(linux, Some("tilo")),
            HostChrome::TiloShell
        );
        assert_eq!(
            HostChrome::detect(linux, Some("GNOME:tilo")),
            HostChrome::TiloShell
        );
        assert_eq!(
            HostChrome::detect(linux, Some("GNOME")),
            HostChrome::InWindow
        );
        assert_eq!(
            HostChrome::detect(windows, None) == HostChrome::Native,
            cfg!(all(windows, feature = "native"))
        );
        assert!(!HostChrome::detect(linux, Some("tilo")).draws_in_window());
        let mac = HostChrome::detect(PlatformConventions::new(Platform::MacOs), None);
        assert_eq!(
            mac == HostChrome::Native,
            cfg!(all(target_os = "macos", feature = "native"))
        );
    }
}
