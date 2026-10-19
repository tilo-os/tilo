//! The installation's frontend, and what the image writer shares with it.
//!
//! Most of this tree is the guided installation of Tilo from another
//! operating system (`installation`, see apps/imager/Cargo.toml): the
//! installer journey, the guided setup, the folder imports, the capture and
//! transfer of the old system's data and the helpers they start. The Tilo
//! app leaves it out. The image writer itself (`crate::app`) only shares the
//! consumer admissions, the internal-bundle and staging contracts the release
//! catalog validates, the media facades its write job calls and the strings,
//! and sees the prepared installer medium through `journey::prepared_media`,
//! which without `installation` is the empty seam in `journey_absent.rs`.

pub mod account_admission;
#[cfg(feature = "installation")]
pub mod account_transfer;
#[cfg(feature = "installation")]
pub mod actions;
pub mod applications_admission;
#[cfg(target_os = "linux")]
pub mod browser_transfer;
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
#[cfg(feature = "installation")]
mod destination_security;
#[cfg(feature = "installation")]
pub mod flatpak_transfer;
#[cfg(feature = "installation")]
pub mod folders;
pub mod home_admission;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) mod home_capture;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) mod home_capture_helper;
#[cfg(feature = "installation")]
pub mod home_transfer;
#[cfg(feature = "installation")]
pub mod import_actions;
#[cfg(feature = "installation")]
pub mod import_view;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod installed_copy;
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
/// The image writer's view of the prepared installer medium when the
/// installation is not built: no medium can be prepared, so none is pending.
#[cfg(not(feature = "installation"))]
#[path = "journey_absent.rs"]
pub mod journey;
#[cfg(feature = "installation")]
pub mod known_folders;
pub mod migration;
#[cfg(feature = "installation")]
mod migration_durability;
pub mod migration_handoff;
#[cfg(feature = "installation")]
pub(crate) mod migration_intent;
#[cfg(feature = "installation")]
pub mod migration_journal;
#[cfg(feature = "installation")]
mod migration_journal_log;
#[cfg(all(any(target_os = "linux", test), feature = "installation"))]
pub mod migration_native;
#[cfg(feature = "installation")]
pub mod migration_recovery;
#[cfg(feature = "installation")]
mod migration_review;
pub mod original_admission;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_frontend;
#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
mod qualification_selected_grant;
#[cfg(feature = "installation")]
pub mod service_sessions;
#[cfg(feature = "installation")]
pub mod windows_inventory;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub use migration_review::{
    GrantedHomeRoots, GrantedRoot, HomeRootSet, MountedRootChooser, MountedRootSelection, RootKind,
};
#[cfg(feature = "installation")]
mod migration_store;
#[cfg(feature = "installation")]
mod migration_worker;
#[cfg(feature = "installation")]
pub mod model;
#[cfg(feature = "installation")]
mod nofollow;
#[cfg(feature = "installation")]
pub mod prepared_identity;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod prepared_setup;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod prepared_setup_helper;
pub mod staging;
pub(crate) mod strings;
#[cfg(all(test, feature = "installation"))]
mod test_support;
#[cfg(feature = "installation")]
pub mod view;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod user_transfer;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod portable_transfer;

#[cfg(target_os = "linux")]
pub mod media_setup;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod browser_transfer_helper;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod portable_applications_helper;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod portable_home_helper;
#[cfg(all(target_os = "linux", feature = "installation"))]
pub mod portable_home_import;
#[cfg(target_os = "linux")]
pub mod portable_home_media;

#[cfg(all(target_os = "linux", feature = "installation"))]
pub(crate) mod read_media;

#[cfg(feature = "capture-worker")]
pub(crate) mod retained_source_io;

#[cfg(feature = "capture-worker")]
pub(crate) mod retained_target_projection;

#[cfg(feature = "installation")]
mod desktop_journey;

#[cfg(feature = "installation")]
mod journey_navigation;
