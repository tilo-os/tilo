//! Who draws the window's frame: the desktop, or the app itself. `tilo_presentation` observes what the
//! desktop does; this decides what the app does with that, the same way on every host: a host whose
//! chrome the app must draw itself ([`HostChrome::InWindow`]) draws the frame only where the desktop
//! leaves decorations to the client, and every host with chrome of its own (the Tilo shell's, the macOS
//! menu bar's) draws none.

use super::HostChrome;

/// Whether the app draws its own frame, once the first configure has settled `xdg-decoration`
/// (`server_side`); `None` until then.
pub(crate) fn app_draws_frame(chrome: HostChrome, server_side: Option<bool>) -> Option<bool> {
    match (chrome, server_side) {
        (HostChrome::InWindow, None) => None,
        (HostChrome::InWindow, Some(server_side)) => Some(!server_side),
        (HostChrome::TiloShell | HostChrome::Native, _) => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::app_draws_frame;
    use crate::host_chrome::HostChrome;

    #[test]
    fn only_client_side_decoration_outside_tilo_gets_the_bar() {
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, None),
            None,
            "wait for the first configure"
        );
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, Some(false)),
            Some(true),
            "GNOME leaves the frame to the app"
        );
        assert_eq!(
            app_draws_frame(HostChrome::InWindow, Some(true)),
            Some(false),
            "KDE and X11 window managers draw it"
        );
        for server_side in [None, Some(false), Some(true)] {
            assert_eq!(
                app_draws_frame(HostChrome::TiloShell, server_side),
                Some(false),
                "the Tilo shell owns the chrome"
            );
            assert_eq!(
                app_draws_frame(HostChrome::Native, server_side),
                Some(false),
                "the host's own chrome draws it"
            );
        }
    }
}
