//! Reading another computer's accounts, mail and personal data.
//!
//! Two families live here, both shared by the Tilo installer and by a laptop
//! command that drives the same sources without the installer's desktop stack:
//!
//! - [`service`]: Outline, Linear and mail accounts reached with a key or a
//!   password the person types once.
//! - [`thunderbird`]: Thunderbird profiles, their mail folders, address books
//!   and local calendars read from the profile's own files.
//!
//! [`pinned`] holds the pinned, never-followed directory access both read
//! through; [`destination_security`] is the matching check for destinations the
//! installer writes. Nothing here links a desktop stack, so a plain Linux or
//! macOS machine builds it.

pub mod bounded;
pub mod destination_security;
pub mod pinned;
pub mod service;
pub mod thunderbird;
pub mod vault;
