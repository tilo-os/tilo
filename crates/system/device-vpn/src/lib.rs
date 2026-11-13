//! Your devices as a VPN: one of a person's devices routes another's
//! traffic, over the iroh connection their shells already share
//! (docs/device-vpn.md). The same root helper holds the kill switch of the
//! person's `NetworkManager` VPN profiles.

pub mod net;
pub mod packet;
pub mod profile;
pub mod protocol;

#[cfg(target_os = "linux")]
pub mod fdpass;

#[cfg(feature = "tunnel")]
pub mod tunnel;

#[cfg(feature = "session")]
pub mod controller;
#[cfg(feature = "session")]
pub mod profile_block;

#[cfg(all(feature = "service", target_os = "linux"))]
pub mod service;
