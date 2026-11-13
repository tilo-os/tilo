//! Where History, Search and Undo appear when an app runs outside the Tilo desktop: in the host
//! operating system's own chrome. Apps never describe menus or draw chrome.

pub(crate) mod edit;
#[cfg(any(feature = "native", test))]
pub(crate) mod frame;
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
