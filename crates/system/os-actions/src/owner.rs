//! Which process may act on this machine's system surfaces.
//!
//! Brightness, audio, networking, Bluetooth, power, session lifecycle, time,
//! accounts, printing, firmware and virtual input devices all belong to the
//! machine, not to whichever Tilo shell happens to be running. Only the
//! compositor that drives the machine's display on the DRM backend, with the
//! seat, owns them. A shell nested in someone's desktop (winit), a headless
//! shell, and the hosted product all run on a machine that something else
//! owns, so they must never reach it.
//!
//! The answer is set once, where the compositor chooses its backend
//! ([`establish`]), and inherited by the processes it starts through
//! [`ENV`]. Every host-touching executor in this crate (subprocesses, D-Bus
//! calls, sysfs and i2c writes, uinput devices, the logout request) asks
//! [`admit`] first. A process that was never told is [`SystemSurfaces::Hosted`]:
//! failing closed means a tool started by hand on a developer's desktop cannot
//! reach that desktop either.
//!
//! This is a guard against acting on the wrong machine by accident, not a
//! security boundary: any process can set its own environment. Authority stays
//! where it already lives (polkit, logind, file permissions and the app
//! sandbox, which starts every sandboxed app with a cleared environment).

use std::sync::atomic::{AtomicU8, Ordering};

/// Environment variable carrying the answer to child processes. Its value is
/// `owned` or `hosted`; anything else, or absence, means hosted.
pub const ENV: &str = "TILO_SYSTEM_SURFACES";

/// The user-facing reason a system action is unavailable on a shell that
/// does not own the machine.
pub const HOST_MANAGED_REASON: &str = "Managed by the host system";

/// Stable machine-readable code for [`HostManaged`], used where refusals
/// carry a code (the action catalogue's `Availability::Unavailable`).
pub const HOST_MANAGED_CODE: &str = "host_managed";

/// Whether this process owns the machine's system surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemSurfaces {
    /// The compositor on the DRM backend with the seat, or a process it
    /// started: system actions act on this machine.
    Owned,
    /// A nested, headless or hosted shell, or a process nobody told:
    /// something else owns this machine and system actions are refused.
    Hosted,
}

impl SystemSurfaces {
    /// Whether system actions may run.
    #[must_use]
    pub const fn is_owned(self) -> bool {
        matches!(self, Self::Owned)
    }

    /// [`ENV`]'s value for this answer.
    #[must_use]
    pub const fn env_value(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Hosted => "hosted",
        }
    }

    /// Parse [`ENV`]'s value; anything but `owned` is hosted.
    #[must_use]
    pub fn from_env_value(value: Option<&str>) -> Self {
        match value.map(str::trim) {
            Some("owned") => Self::Owned,
            _ => Self::Hosted,
        }
    }

    /// `Ok` when owned, the typed refusal otherwise.
    ///
    /// # Errors
    ///
    /// [`HostManaged`] when this process does not own the machine.
    pub const fn require(self) -> Result<(), HostManaged> {
        match self {
            Self::Owned => Ok(()),
            Self::Hosted => Err(HostManaged),
        }
    }
}

/// The typed refusal of a system action on a shell that does not own the
/// machine. Converts into the crate's `String` errors with
/// [`HOST_MANAGED_REASON`] as its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Managed by the host system")]
pub struct HostManaged;

impl From<HostManaged> for String {
    fn from(refusal: HostManaged) -> Self {
        refusal.to_string()
    }
}

const UNSET: u8 = 0;
const OWNED: u8 = 1;
const HOSTED: u8 = 2;

static STATE: AtomicU8 = AtomicU8::new(UNSET);

const fn encode(surfaces: SystemSurfaces) -> u8 {
    match surfaces {
        SystemSurfaces::Owned => OWNED,
        SystemSurfaces::Hosted => HOSTED,
    }
}

/// Record this process's answer and publish it to the processes it starts.
///
/// Called by the compositor once, from its single-threaded startup, where it
/// chooses the backend. A nested shell started from inside an owning Tilo
/// session inherits `owned` and overwrites it here, so its own children are
/// hosted too.
#[allow(
    clippy::disallowed_methods,
    reason = "called from the compositor's startup before any thread that reads the environment; \
              children inherit the answer through it"
)]
pub fn establish(surfaces: SystemSurfaces) {
    STATE.store(encode(surfaces), Ordering::Release);
    // SAFETY: called from the compositor's startup, before it spawns any
    // thread that reads or writes the environment; the only thread running
    // then (font warm-up) never touches it.
    unsafe { std::env::set_var(ENV, surfaces.env_value()) };
}

/// This process's answer: the one [`establish`]ed, else the one inherited
/// through [`ENV`], else [`SystemSurfaces::Hosted`].
#[must_use]
pub fn current() -> SystemSurfaces {
    // A test never inherits the answer of the machine running it.
    #[cfg(test)]
    {
        testing::surfaces().unwrap_or(SystemSurfaces::Hosted)
    }
    #[cfg(not(test))]
    {
        established()
    }
}

#[cfg_attr(test, allow(dead_code, reason = "tests answer per thread instead"))]
fn established() -> SystemSurfaces {
    if STATE.load(Ordering::Acquire) == UNSET {
        let inherited = SystemSurfaces::from_env_value(std::env::var(ENV).ok().as_deref());
        // A racing `establish` wins: only replace the unset state.
        let _ = STATE.compare_exchange(
            UNSET,
            encode(inherited),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }
    if STATE.load(Ordering::Acquire) == OWNED {
        SystemSurfaces::Owned
    } else {
        SystemSurfaces::Hosted
    }
}

/// Gate one host-touching executor. Every subprocess, D-Bus call, sysfs or
/// i2c write and uinput device in this crate passes through here first, so
/// nothing on a non-owning shell reaches the machine.
///
/// # Errors
///
/// [`HostManaged`] (as its message) when this process does not own the
/// machine. In this crate's tests a recording executor may be installed
/// instead; it records `executor` and refuses, so no test reaches the host.
pub(crate) fn admit(executor: &str) -> Result<(), String> {
    current().require()?;
    #[cfg(test)]
    testing::intercept(executor)?;
    let _ = executor;
    Ok(())
}

/// Per-thread overrides for this crate's tests: which answer applies and
/// whether executors are recorded instead of run.
#[cfg(test)]
pub(crate) mod testing {
    use std::cell::{Cell, RefCell};

    use super::SystemSurfaces;

    thread_local! {
        static SURFACES: Cell<Option<SystemSurfaces>> = const { Cell::new(None) };
        static RECORDER: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    }

    pub(crate) fn surfaces() -> Option<SystemSurfaces> {
        SURFACES.with(Cell::get)
    }

    /// Run `body` with `surfaces` in force on this thread and the real
    /// executors in place (only for tests that run harmless local tools).
    pub(crate) fn with_surfaces<R>(surfaces: SystemSurfaces, body: impl FnOnce() -> R) -> R {
        let previous = SURFACES.with(|cell| cell.replace(Some(surfaces)));
        let result = body();
        SURFACES.with(|cell| cell.set(previous));
        result
    }

    /// Run `body` with `surfaces` in force and every executor replaced by a
    /// recorder, returning the executors reached.
    pub(crate) fn recording(surfaces: SystemSurfaces, body: impl FnOnce()) -> Vec<String> {
        RECORDER.with(|cell| *cell.borrow_mut() = Some(Vec::new()));
        with_surfaces(surfaces, body);
        RECORDER.with(|cell| cell.borrow_mut().take().unwrap_or_default())
    }

    pub(super) fn intercept(executor: &str) -> Result<(), String> {
        RECORDER.with(|cell| match cell.borrow_mut().as_mut() {
            Some(reached) => {
                reached.push(executor.to_owned());
                Err(format!("recorded {executor}"))
            }
            None => Ok(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_owned_answer_owns_the_machine() {
        assert_eq!(
            SystemSurfaces::from_env_value(Some("owned")),
            SystemSurfaces::Owned
        );
        for value in [None, Some(""), Some("hosted"), Some("OWNED"), Some("yes")] {
            assert_eq!(
                SystemSurfaces::from_env_value(value),
                SystemSurfaces::Hosted,
                "{value:?}"
            );
        }
        assert_eq!(SystemSurfaces::Hosted.require(), Err(HostManaged));
        assert_eq!(String::from(HostManaged), HOST_MANAGED_REASON);
        assert!(SystemSurfaces::Owned.require().is_ok());
    }
}
