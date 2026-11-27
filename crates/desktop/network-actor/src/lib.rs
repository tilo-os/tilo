//! Who a device is and what the person's devices say to each other: the portable half of the
//! device fabric.
//!
//! The transport is the device's daemon's: it holds the endpoint and the key, and the desktop
//! session reaches the workspaces and the person's other devices through it. What is portable
//! stays here so a role, a page and a desktop name a device, a stamp and an exchange the same way.

pub mod behaviour;
mod i18n {
    tilo_ui_base::embedded_catalog!(pub(crate) fn, ["en-US", "de-DE"]);
}
pub mod fabric;

pub use tilo_desktop_model::WorkspaceId;
/// A device's identity: the certificate it presents and the key it signs with.
pub use tilo_identity::identity_store::DeviceIdentity;
