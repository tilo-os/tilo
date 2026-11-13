//! The installation's frontend, and what the image writer shares with it.

#[cfg(feature = "installation")]
pub mod actions;
#[cfg(feature = "installation")]
pub mod controller;
#[cfg(feature = "installation")]
pub mod desktop_catalog;
#[cfg(feature = "installation")]
mod desktop_continuation;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) mod desktop_delivery;
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
#[cfg(all(test, windows, feature = "installation"))]
mod destination_security_windows_tests;
#[cfg(feature = "installation")]
pub mod folders;
#[cfg(any(feature = "installation", feature = "destination"))]
pub(crate) mod hardware_text;
#[cfg(all(test, target_os = "linux", feature = "installation"))]
mod home_transfer_promotion_tests;
#[cfg(feature = "installation")]
pub mod import_actions;
#[cfg(feature = "installation")]
mod import_rows;
#[cfg(feature = "installation")]
pub mod import_view;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod installed_migration_helper;

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
#[cfg(not(feature = "installation"))]
#[path = "journey_absent.rs"]
pub mod journey;
#[cfg(feature = "installation")]
pub mod migration_recovery;
#[cfg(feature = "installation")]
mod migration_review;
pub mod original_admission;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_account_destination;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_frontend;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_selected_grant;
#[cfg(feature = "installation")]
pub mod windows_inventory;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub use migration_review::{
    GrantedHomeRoots, GrantedRoot, HomeRootSet, MountedRootChooser, MountedRootSelection, RootKind,
};
#[cfg(feature = "installation")]
pub mod model;
pub mod staging;
pub(crate) mod strings;
#[cfg(all(test, feature = "installation"))]
mod test_support;
#[cfg(feature = "installation")]
pub mod view;

#[cfg(target_os = "linux")]
pub mod media_setup;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod portable_applications_helper;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) mod read_media;

#[cfg(feature = "installation")]
mod desktop_journey;

#[cfg(feature = "installation")]
mod journey_navigation;
