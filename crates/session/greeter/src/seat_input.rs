//! Tell the boot assessment that someone could type or touch to sign in.
//!
//! A boot on trial counts as good once the sign-in screen holds and a
//! keyboard or touch screen reaches it (`os/usr/libexec/tilo-boot-health`).
//! The kiosk compositor advertises on its `wl_seat` what it has opened: it
//! sets the keyboard capability once it has a keyboard and the touch
//! capability once it has a touch screen, and routes their events to this
//! window. So the greeter watches its seats and, the first time one carries
//! either, writes the kind to the record file `tilo-cage-session` names in
//! [`RECORD_ENV`]. A pointer alone writes nothing: nobody can enter a
//! passcode with it.
//!
//! The record is removed when the greeter starts, so it always describes the
//! greeter running now, and replaced atomically, so the health check never
//! reads half of it.

use std::path::{Path, PathBuf};

use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum};

/// Where the record goes; unset outside a greeter session.
pub const RECORD_ENV: &str = "TILO_GREETER_INPUT_RECORD";

/// The input a seat with `capabilities` gives the sign-in screen, if any a
/// person can sign in with.
#[must_use]
pub fn kind(capabilities: wl_seat::Capability) -> Option<&'static str> {
    if capabilities.contains(wl_seat::Capability::Keyboard) {
        Some("keyboard")
    } else if capabilities.contains(wl_seat::Capability::Touch) {
        Some("touch")
    } else {
        None
    }
}

/// Replace the record at `path` with `kind`.
///
/// # Errors
/// When the record's directory cannot be written.
pub fn write_record(path: &Path, kind: &str) -> std::io::Result<()> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".new");
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, format!("{kind}\n"))?;
    std::fs::rename(&staged, path)
}

/// Watches the seats until one gives the sign-in screen a keyboard or touch
/// screen, then records it once.
#[derive(Debug)]
pub struct Seats {
    record: PathBuf,
    recorded: Option<&'static str>,
}

impl Seats {
    #[must_use]
    pub fn new(record: PathBuf) -> Self {
        Self {
            record,
            recorded: None,
        }
    }

    /// What has been recorded, if anything.
    #[must_use]
    pub fn recorded(&self) -> Option<&'static str> {
        self.recorded
    }

    /// One seat's capabilities as the compositor announced them.
    pub fn capabilities(&mut self, capabilities: wl_seat::Capability) {
        if self.recorded.is_some() {
            return;
        }
        let Some(kind) = kind(capabilities) else {
            return;
        };
        match write_record(&self.record, kind) {
            Ok(()) => {
                tracing::info!(kind, "greeter input: the sign-in screen has a {kind}");
                self.recorded = Some(kind);
            }
            Err(error) => {
                tracing::warn!(%error, record = %self.record.display(), "greeter input: could not write the record");
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Seats {
    fn event(
        _: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
            && interface == "wl_seat"
        {
            registry.bind::<wl_seat::WlSeat, _, _>(name, version.min(5), qh, ());
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Seats {
    fn event(
        state: &mut Self,
        _: &wl_seat::WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            state.capabilities(capabilities);
        }
    }
}

/// Start watching, when this greeter has a record to keep.
pub fn start() {
    let Some(record) = std::env::var_os(RECORD_ENV).map(PathBuf::from) else {
        return;
    };
    if let Err(error) = std::fs::remove_file(&record)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, record = %record.display(), "greeter input: could not clear the record");
    }
    let Ok(connection) = Connection::connect_to_env() else {
        tracing::warn!("greeter input: no compositor connection");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("greeter-input".into())
        .spawn(move || {
            let mut queue = connection.new_event_queue::<Seats>();
            let _registry = connection.display().get_registry(&queue.handle(), ());
            let mut seats = Seats::new(record);
            while seats.recorded().is_none() {
                if queue.blocking_dispatch(&mut seats).is_err() {
                    tracing::warn!("greeter input: the compositor connection closed");
                    return;
                }
            }
        });
    if spawned.is_err() {
        tracing::warn!("greeter input: could not watch the seats");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wl_seat::Capability;

    #[test]
    fn a_keyboard_or_touch_screen_is_input_a_person_can_sign_in_with() {
        assert_eq!(kind(Capability::Keyboard), Some("keyboard"));
        assert_eq!(
            kind(Capability::Pointer | Capability::Keyboard),
            Some("keyboard")
        );
        assert_eq!(kind(Capability::Touch), Some("touch"));
        assert_eq!(kind(Capability::Pointer | Capability::Touch), Some("touch"));
    }

    #[test]
    fn a_mouse_alone_is_not() {
        assert_eq!(kind(Capability::Pointer), None);
        assert_eq!(kind(Capability::empty()), None);
    }

    #[test]
    fn a_mouse_only_seat_writes_no_record() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = dir.path().join("input");
        let mut seats = Seats::new(record.clone());
        seats.capabilities(Capability::Pointer);
        seats.capabilities(Capability::empty());
        assert_eq!(seats.recorded(), None);
        assert!(!record.exists());
    }

    #[test]
    fn the_first_keyboard_or_touch_screen_is_recorded_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let record = dir.path().join("input");
        let mut seats = Seats::new(record.clone());
        seats.capabilities(Capability::Pointer);
        seats.capabilities(Capability::Pointer | Capability::Touch);
        assert_eq!(seats.recorded(), Some("touch"));
        assert_eq!(std::fs::read_to_string(&record).expect("record"), "touch\n");
        seats.capabilities(Capability::Keyboard);
        assert_eq!(std::fs::read_to_string(&record).expect("record"), "touch\n");
        assert!(!dir.path().join("input.new").exists());
    }

    #[test]
    fn the_record_names_what_the_health_check_reads() {
        let script = include_str!("../../../../os/usr/libexec/tilo-boot-health");
        assert!(script.contains("\"keyboard\""));
        assert!(script.contains("\"touch\""));
        let session = include_str!("../../../../os/usr/libexec/tilo-cage-session");
        assert!(session.contains(RECORD_ENV));
    }
}
