//! The installation's frontend, and what the image writer shares with it.

#[cfg(feature = "installation")]
pub mod actions;
#[cfg(feature = "installation")]
pub mod desktop_catalog;
#[cfg(feature = "installation")]
pub mod desktop_download;
#[cfg(feature = "installation")]
pub mod desktop_flow;
#[cfg(feature = "installation")]
pub mod desktop_install;
#[cfg(feature = "installation")]
pub mod desktop_probe;
#[cfg(feature = "installation")]
pub mod desktop_worker;
#[cfg(feature = "destination")]
pub mod destination;
#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) mod hardware_text;

pub mod internal_bundle;
#[cfg(feature = "installation")]
mod internal_cache;
#[cfg(feature = "installation")]
pub mod internal_download;
#[cfg(feature = "installation")]
mod internal_download_flow;
#[cfg(feature = "installation")]
mod internal_install_flow;
#[cfg(feature = "installation")]
pub mod journey;
#[cfg(feature = "installation")]
pub mod model;
pub mod staging;
pub(crate) mod strings;
#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) mod support_report_view;

#[cfg(feature = "installation")]
mod desktop_journey;
