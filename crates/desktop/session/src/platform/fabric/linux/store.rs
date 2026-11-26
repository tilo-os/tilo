//! What the person's devices keep of themselves in the person's own workspace.
//!
//! Each device writes what lasts of it under its own key
//! ([`tilo_workspace_service::workspaces::person_workspace::devices`]): what it is ([`ABOUT`]).
//! Its speaker groups are typed actions on the same record
//! ([`tilo_workspace_service::workspaces::person_workspace::speaker`]), and its daemon answers
//! another device's disk unlock from the record itself. The workspace carries all of it between
//! the person's devices, a device that comes up later included.
//!
//! Every value is the postcard bytes of its type, written only when it changed; a value that does
//! not read, or is not true of the device it is under, is left out, and so is a device whose
//! certificate the person's root did not sign or has since revoked.

use serde::de::DeserializeOwned;
use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::person_workspace::devices::Devices;

use super::{DeviceAbout, PersonDevice};
use crate::platform::fabric::DeviceId;

/// What a device is: its [`DeviceAbout`].
pub(crate) const ABOUT: &str = "about";

/// What the person's devices keep of themselves in their record `state`: each device of `person`
/// whose certificate holds, with what it said of itself.
pub(crate) fn kept(state: &Devices, person: &[u8; 32]) -> Vec<PersonDevice> {
    let now = tilo_clock::unix_now_ns();
    state
        .devices
        .iter()
        .filter_map(|(_, kept)| {
            let cert: DeviceCert = decoded(&kept.certificate)?;
            if &cert.user_public_key != person
                || tilo_user_identity::verify_device_cert_at(&cert, now).is_err()
            {
                return None;
            }
            let device = DeviceId::new(hex::encode(cert.device_public_key)).ok()?;
            let fact = |name: &str| kept.facts.get(name);
            let about = fact(ABOUT)
                .and_then(|value| decoded::<DeviceAbout>(value))
                .filter(|about| about.validate().is_ok())
                .unwrap_or_else(|| {
                    DeviceAbout::named(
                        tilo_user_identity::device_name(&cert.device_label).unwrap_or_default(),
                    )
                });
            Some(PersonDevice::new(device, about))
        })
        .collect()
}

/// The value `hex` holds, postcard in hex, when it reads as a `T`.
pub(crate) fn decoded<T: DeserializeOwned>(hex: &str) -> Option<T> {
    tilo_failure::ignored!(
        "a value no build of this person's wrote is not one of their devices' to read",
        postcard::from_bytes(&hex::decode(hex).ok()?).ok()
    )
}
