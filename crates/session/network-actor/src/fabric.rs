//! What the actor carries on its sessions with the person's own devices: the actions it sends
//! them, the drags it carries between them, their shared clock and speaker groups, and the
//! person's devices read as one list. Who a device is and what it says of itself are
//! `tilo-fabric`'s; what continuity hands between devices is `tilo-continuity`'s.

pub mod carried;
pub mod clock;
pub mod devices;
pub mod link;
mod model;
pub mod speaker_group;

pub use carried::{CarriedDrag, CarriedOffer, MAX_CARRIED_OFFERS};
pub use devices::{PersonDevice, PersonDevices};
pub use model::*;
