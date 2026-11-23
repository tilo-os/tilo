//! The sibling factor of the system volume: what enrolling it keeps in the
//! LUKS2 header ([`token`], [`material`]), how it is enrolled ([`enrol`]),
//! and how early boot reaches the owner's other devices ([`network`],
//! [`exchange`]). The boot attempt itself is [`crate::boot::sibling`].

pub mod enrol;
pub mod exchange;
pub mod material;
pub mod network;
pub mod token;
