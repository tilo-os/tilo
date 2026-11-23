//! What continues across a person's devices, each a service of actions over their device fabric
//! ([`tilo_fabric`]): the clipboard ([`clipboard`]), notifications ([`notifications`]), Wi-Fi
//! passphrases ([`wifi`]) and Bluetooth accessories ([`accessories`]), under the person's
//! Continuity switches ([`Switches`]).
//!
//! Each service publishes its own provider. What reaches another device is an action call to that
//! device's copy of the same provider; what needs the desktop here (the clipboard history it shows,
//! a mirrored banner, a notice) is a call to the session on this device, which a device without a
//! session refuses.

pub mod accessories;
pub mod clipboard;
pub mod notifications;
pub mod secrets;
mod switches;
pub mod wifi;

pub use switches::Switches;
