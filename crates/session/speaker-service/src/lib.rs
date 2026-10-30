//! Speaker groups and remote audio across one person's devices.
//!
//! A speaker group plays one source on several of the person's devices in
//! time ([`groups`]), measured and trimmed by an acoustic sweep
//! ([`calibration`]); remote audio carries a device's playback or
//! microphone to another ([`remote_audio`]); Settings reaches all of it
//! through [`settings`]. Everything goes over the person's device fabric,
//! which the service reaches through a [`tilo_device_fabric::link::FabricLink`].

pub mod calibration;
pub mod groups;
pub mod remote_audio;
pub mod settings;

mod i18n {
    ui::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);
}
