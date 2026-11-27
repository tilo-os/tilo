//! Where History, Search and Undo appear when an app runs outside the Tilo shell: in the host
//! operating system's own chrome. Apps never describe menus or draw chrome.

pub mod edit;
#[cfg(any(feature = "native", test))]
pub mod frame;
#[cfg(any(
    all(target_os = "linux", feature = "native"),
    any(test, feature = "test-support")
))]
pub mod header_bar;
#[cfg(all(target_os = "macos", feature = "native"))]
pub mod macos;
#[cfg(any(
    all(any(target_os = "macos", windows), feature = "native"),
    any(test, feature = "test-support")
))]
pub mod menus;
pub mod model;
#[cfg(any(feature = "native", test))]
mod owner;
#[cfg(all(windows, feature = "native"))]
pub(crate) mod windows;
#[cfg(any(all(windows, feature = "native"), any(test, feature = "test-support")))]
pub mod windows_items;
pub mod zoom;

#[cfg(any(feature = "native", test))]
pub use owner::HostChrome;
