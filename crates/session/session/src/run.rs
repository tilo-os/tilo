//! What a session keeps to itself when it runs in a window of the desktop
//! it was started from.
//!
//! A nested session lives inside someone's desktop, so it must leave that
//! desktop alone: it serves its desktop services on no bus of it unless a
//! flag names one (a private dbus-daemon), and it never exports its
//! displays with `dbus-update-activation-environment`, which would point the
//! surrounding session's systemd user manager and D-Bus-activated services
//! at its own Wayland socket, and no D-Bus name of its own lands on the
//! host's buses. Its own XDG names stay clear of the host's too: the
//! `wayland-N` socket and the Xwayland display are picked from the free
//! ones, so several nested sessions can run beside a desktop and each other.

use crate::desktop_services::{Bus, Buses};

/// How a session shows itself: on a bare TTY, with no display at all, or in
/// a window of the desktop it is started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// On a bare TTY ([`Session::drm`](crate::Session::drm)).
    Drm,
    /// With no display, for tests and screenshots
    /// ([`Session::headless`](crate::Session::headless)).
    Headless,
    /// In a window of the desktop it is started from
    /// ([`Session::nested`](crate::Session::nested)).
    Nested,
}

impl Backend {
    /// The backend `--backend` names, if it names one.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "drm" => Some(Self::Drm),
            "headless" => Some(Self::Headless),
            "nested" => Some(Self::Nested),
            _ => None,
        }
    }
}

/// What a session with a backend reaches of the machine around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
    /// The buses its desktop services serve on.
    pub buses: Buses,
    /// Whether its displays are exported to the systemd user manager and
    /// D-Bus activation.
    pub export_displays: bool,
}

impl Reach {
    /// What `backend` reaches of `buses` as asked for: a nested session
    /// keeps to the buses a flag names, and exports nothing.
    #[must_use]
    pub fn new(backend: Backend, buses: Buses) -> Self {
        match backend {
            Backend::Nested => Self {
                buses: Buses {
                    session: named(buses.session),
                    system: named(buses.system),
                },
                export_displays: false,
            },
            Backend::Drm | Backend::Headless => Self {
                buses,
                export_displays: true,
            },
        }
    }

    /// The session bus a flag named: what a command started in the session
    /// reaches the portal and the notifications daemon by, rather than the
    /// bus its own environment names.
    #[must_use]
    pub fn session_bus(&self) -> Option<&str> {
        match &self.buses.session {
            Bus::Address(address) => Some(address),
            Bus::Default | Bus::None => None,
        }
    }
}

/// `bus` when a flag named it, else no bus.
fn named(bus: Bus) -> Bus {
    match bus {
        Bus::Address(_) => bus,
        Bus::Default | Bus::None => Bus::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_buses() -> Buses {
        Buses {
            session: Bus::Default,
            system: Bus::Default,
        }
    }

    #[test]
    fn a_nested_session_serves_no_bus_of_the_desktop_it_is_in() {
        let reach = Reach::new(Backend::Nested, default_buses());
        assert_eq!(
            reach.buses,
            Buses {
                session: Bus::None,
                system: Bus::None
            }
        );
    }

    #[test]
    fn a_nested_session_never_exports_its_displays() {
        let reach = Reach::new(Backend::Nested, default_buses());
        assert!(!reach.export_displays);
    }

    #[test]
    fn a_nested_session_serves_the_bus_a_flag_names() {
        let buses = Buses {
            session: Bus::Address("unix:path=/run/user/1000/tilo-nested/bus".into()),
            system: Bus::None,
        };
        let reach = Reach::new(Backend::Nested, buses.clone());
        assert_eq!(reach.buses, buses);
        assert_eq!(
            reach.session_bus(),
            Some("unix:path=/run/user/1000/tilo-nested/bus")
        );
        assert!(!reach.export_displays);
    }

    #[test]
    fn a_session_on_a_tty_or_without_a_display_exports_its_displays() {
        for backend in [Backend::Drm, Backend::Headless] {
            let reach = Reach::new(backend, default_buses());
            assert_eq!(reach.buses, default_buses());
            assert_eq!(reach.session_bus(), None);
            assert!(reach.export_displays);
        }
    }

    #[test]
    fn the_backends_are_the_ones_the_flag_names() {
        assert_eq!(Backend::named("drm"), Some(Backend::Drm));
        assert_eq!(Backend::named("headless"), Some(Backend::Headless));
        assert_eq!(Backend::named("nested"), Some(Backend::Nested));
        assert_eq!(Backend::named("winit"), None);
    }
}
