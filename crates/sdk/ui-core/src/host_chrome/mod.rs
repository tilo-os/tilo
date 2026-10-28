//! Where History, Search and Undo appear when an app runs outside the Tilo
//! desktop: in the host operating system's own chrome.
//!
//! Apps never describe menus or draw chrome. The host reads one command
//! model ([`model::host_commands`]) built from its own resources and the
//! app's declared actions; every item is an [`tilo_actions::ActionBinding`]
//! that enters the same local door as the palette and assistive technology.

pub(crate) mod edit;
#[cfg(any(all(target_os = "linux", feature = "native"), test))]
pub(crate) mod header_bar;
#[cfg(all(target_os = "macos", feature = "native"))]
pub(crate) mod macos;
#[cfg(any(all(any(target_os = "macos", windows), feature = "native"), test))]
pub(crate) mod menus;
#[cfg(any(feature = "native", test))]
pub(crate) mod model;
#[cfg(any(feature = "native", test))]
mod owner;
#[cfg(all(windows, feature = "native"))]
pub(crate) mod windows;
#[cfg(any(all(windows, feature = "native"), test))]
pub(crate) mod windows_items;
#[cfg(feature = "native")]
pub(crate) mod zoom;

#[cfg(any(feature = "native", test))]
pub(crate) use owner::HostChrome;
