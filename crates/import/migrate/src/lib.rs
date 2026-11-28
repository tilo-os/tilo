//! The one migration engine ([`engine`]) and the readers of other products' data its sources
//! open:
//!
//! - [`service`]: Outline, Linear, Forgejo and mail accounts reached with a key or a password the
//!   person types once.
//! - [`thunderbird`]: Thunderbird profiles, their mail folders, address books and local calendars
//!   read from the profile's own files.
//! - [`browsers`]: where a home keeps its browsers and the passwords they saved.
//! - [`known_folders`]: the familiar folders of a home (Documents, Pictures, …) as the system
//!   names them.
//! - [`old_system`]: Windows, macOS or Linux left on a disk, and the homes it kept.
//!
//! [`pinned`] holds the pinned, never-followed directory access the readers read through;
//! [`destination_security`] is the matching check for destinations the installer writes. Nothing
//! here links a desktop stack, so a plain Linux or macOS machine builds it.

pub mod bounded;
pub mod browsers;
pub mod destination_security;
pub mod engine;
pub mod known_folders;
pub mod old_system;
pub mod pinned;
pub mod provider;
pub mod service;
pub mod thunderbird;
pub mod vault;
