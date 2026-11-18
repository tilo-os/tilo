//! The person's certified devices, as a role reaches them: a role keeps the shared link, asks it
//! what the person's other devices said, sends one of them an action and shares what this device
//! has with them. The desktop's session implements it over its peer connection ([`linux`]); a host
//! without that connection hands out none.
//!
//! Who a device is, the stamp a fact carries and the fresh ids that name an exchange are portable
//! ([`tilo_network_actor::fabric`]); what the fabric converges is the desktop's until the settings schema it
//! merges with is portable too.

pub use tilo_network_actor::fabric::{DeviceId, FabricStamp, nonzero};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
