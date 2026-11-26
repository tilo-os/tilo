//! What a session keeps to itself when it runs in a window of the desktop it was started from.
//! A nested session lives inside someone's desktop, so it must leave that desktop alone: it serves
//! its desktop services on the session bus it was started on, as a login does — the harness starts
//! a private dbus-daemon for it — and reaches that desktop's system bus only when a flag names one,
//! so no D-Bus name of its own lands on the host's system bus. It never exports its displays with
//! `dbus-update-activation-environment`, which would point the surrounding session's systemd user
//! manager and D-Bus-activated services at its own Wayland socket. Its own XDG names stay clear of
//! the host's too: the `wayland-N` socket and the Xwayland display are picked from the free ones,
//! so several nested sessions can run beside a desktop and each other.

use crate::desktop_services::{Bus, Buses};

/// How a session shows itself: on a bare TTY, with no display at all, or in a window of the desktop
/// it is started from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// On a bare TTY ([`Session::drm`](crate::Session::drm)).
    Drm,
    /// With no display, for tests and screenshots
    /// ([`Session::headless`](crate::Session::headless)).
    Headless,
    /// In a window of the desktop it is started from ([`Session::nested`](crate::Session::nested)).
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
    /// Whether its displays are exported to the systemd user manager and D-Bus activation.
    pub export_displays: bool,
}

impl Reach {
    /// What `backend` reaches of `buses` as asked for, for a session told whether it owns the
    /// machine ([`tilo_machine`]): a session that runs inside another desktop owns none of
    /// the machine's daemons, so it registers no agent on that desktop's system bus and exports no
    /// displays to it.
    #[must_use]
    pub fn of(backend: Backend, buses: Buses, owns_the_machine: bool) -> Self {
        if matches!(backend, Backend::Nested) || !owns_the_machine {
            return Self {
                buses: Buses {
                    session: buses.session,
                    system: named(buses.system),
                },
                export_displays: false,
            };
        }
        Self {
            buses,
            export_displays: true,
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

    tilo_test_env::scenario! {
        fn a_sessions_flags_and_seat_decide_its_buses_its_displays_and_its_backends;

        phase a_nested_session_serves_the_session_bus_it_was_started_on_and_no_system_bus;
        {
            let reach = Reach::of(Backend::Nested, default_buses(), true);
            assert_eq!(
                reach.buses,
                Buses {
                    session: Bus::Default,
                    system: Bus::None
                }
            );
            assert!(!reach.export_displays);
        }

        phase a_nested_session_serves_the_buses_a_flag_names;
        {
            let buses = Buses {
                session: Bus::Address("unix:path=/run/user/1000/tilo-nested/bus".into()),
                system: Bus::Address("unix:path=/run/user/1000/tilo-nested/system-bus".into()),
            };
            let reach = Reach::of(Backend::Nested, buses.clone(), true);
            assert_eq!(reach.buses, buses);
            assert!(!reach.export_displays);
        }

        phase a_session_on_a_tty_or_without_a_display_exports_its_displays;
        {
            for backend in [Backend::Drm, Backend::Headless] {
                let reach = Reach::of(backend, default_buses(), true);
                assert_eq!(reach.buses, default_buses());
                assert!(reach.export_displays);
            }
        }

        phase a_session_that_owns_no_machine_serves_its_session_bus_and_no_system_bus;
        {
            // A headless session told it is hosted (TILO_MACHINE=hosted, a test run beside a live
            // desktop) draws without a display and serves the session bus it was started on, which
            // the harness gives it a private one: the desktop around it is the one that owns the
            // machine's daemons.
            let hosted = Reach::of(Backend::Headless, default_buses(), false);
            assert_eq!(
                hosted.buses,
                Buses {
                    session: Bus::Default,
                    system: Bus::None
                }
            );
            assert!(!hosted.export_displays);
            // A session that owns the machine is unaffected by the switch, drawing on a tty or
            // without a display.
            let owned = Reach::of(Backend::Headless, default_buses(), true);
            assert_eq!(owned.buses, default_buses());
            assert!(owned.export_displays);
        }

        phase the_backends_are_the_ones_the_flag_names;
        {
            assert_eq!(Backend::named("drm"), Some(Backend::Drm));
            assert_eq!(Backend::named("headless"), Some(Backend::Headless));
            assert_eq!(Backend::named("nested"), Some(Backend::Nested));
            assert_eq!(Backend::named("winit"), None);
        }
    }
}
