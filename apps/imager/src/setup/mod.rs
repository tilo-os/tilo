pub mod account_admission;
pub mod account_transfer;
pub mod actions;
pub mod applications_admission;
#[cfg(target_os = "linux")]
pub mod browser_transfer;
pub mod controller;
pub mod desktop_catalog;
mod desktop_continuation;
#[cfg(target_os = "linux")]
pub(crate) mod desktop_delivery;
pub mod desktop_download;
pub mod desktop_flow;
pub mod desktop_install;
pub mod desktop_probe;
pub mod desktop_worker;
mod destination_security;
pub mod flatpak_transfer;
pub mod folders;
pub mod home_admission;
#[cfg(target_os = "linux")]
pub(crate) mod home_capture;
#[cfg(target_os = "linux")]
pub(crate) mod home_capture_helper;
pub mod home_transfer;
pub mod import_actions;
pub mod import_view;
#[cfg(target_os = "linux")]
pub mod installed_copy;
#[cfg(target_os = "linux")]
pub mod installed_migration_helper;

pub mod internal_bundle;
mod internal_cache;
pub mod internal_download;
mod internal_download_flow;
mod internal_install_flow;
pub mod journey;
pub mod known_folders;
pub mod migration;
mod migration_durability;
pub mod migration_handoff;
pub(crate) mod migration_intent;
pub mod migration_journal;
mod migration_journal_log;
#[cfg(any(target_os = "linux", test))]
pub mod migration_native;
pub mod migration_recovery;
mod migration_review;
pub mod original_admission;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_frontend;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_selected_grant;
pub mod service_sessions;
pub mod windows_inventory;
#[cfg(target_os = "linux")]
pub use migration_review::{
    GrantedHomeRoots, GrantedRoot, HomeRootSet, MountedRootChooser, MountedRootSelection, RootKind,
};
mod migration_store;
mod migration_worker;
pub mod model;
mod nofollow;
pub mod prepared_identity;
#[cfg(target_os = "linux")]
pub mod prepared_setup;
#[cfg(target_os = "linux")]
pub mod prepared_setup_helper;
pub mod staging;
pub(crate) mod strings;
#[cfg(test)]
mod test_support;
pub mod view;

#[cfg(target_os = "linux")]
pub mod user_transfer;

#[cfg(target_os = "linux")]
pub mod portable_transfer;

#[cfg(target_os = "linux")]
pub mod media_setup;

#[cfg(target_os = "linux")]
pub mod browser_transfer_helper;
#[cfg(target_os = "linux")]
pub mod portable_applications_helper;
#[cfg(target_os = "linux")]
pub mod portable_home_helper;
#[cfg(target_os = "linux")]
pub mod portable_home_import;
#[cfg(target_os = "linux")]
pub mod portable_home_media;

#[cfg(target_os = "linux")]
pub(crate) mod read_media;

#[cfg(feature = "capture-worker")]
pub(crate) mod retained_source_io;

#[cfg(feature = "capture-worker")]
pub(crate) mod retained_target_projection;

mod desktop_journey;

mod journey_navigation;
