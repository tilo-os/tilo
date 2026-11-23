//! A person's devices as one, on each device: who is here and on this network, the lock that
//! follows the person from device to device, waking a sleeping device, and unlocking another
//! device's disk at boot.
//!
//! Everything here is an action. [`Fabric::provider`] publishes the fabric's resources and actions
//! under [`PROVIDER`]; a booting device that holds no certificate yet reaches the unlock and
//! enrolment actions under [`UNLOCK_SCOPE`] ([`Fabric::unlock_scope`]). A call to another of the
//! person's devices is an action call addressed `devices/<id>/…`, carried by the [`Invoker`] the
//! host hands in: the crate holds no endpoint and speaks no protocol of its own. Services built on
//! the fabric (clipboard, notifications, Wi-Fi and accessory handoff) publish their own providers
//! the same way.

pub mod actions;
pub mod calls;
mod devices;
mod envelope;
mod error;
mod fabric;
mod id;
pub mod interfaces;
pub mod owner;
mod presence;
#[cfg(feature = "test-support")]
pub mod testing;
pub mod unlock;
pub mod unlock_keys;
pub mod wake;

pub use calls::{
    CANT_NOW, DeviceCall, Facts, Invoker, NoSeat, SeatCalls, cant_now, keep_fact, pack, read_facts,
    spend, unpack,
};
pub use devices::{Device, Devices, ListedDevice};
pub use envelope::{Envelope, MAX_FABRIC_ACTION_AGE_MS, MAX_FABRIC_CLOCK_SKEW_MS, Replays};
pub use error::FabricError;
pub use fabric::*;
pub use id::*;
pub use presence::*;
