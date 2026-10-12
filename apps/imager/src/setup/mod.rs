pub mod account_admission;
pub mod account_transfer;
pub mod actions;
pub mod applications_admission;
#[cfg(unix)]
pub(crate) mod browser_profiles;
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
#[cfg(target_os = "linux")]
pub(crate) mod installed_retained_actor;
pub mod internal_bundle;
mod internal_cache;
pub mod internal_download;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
mod internal_download_flow;
#[cfg(any(target_os = "linux", windows, all(test, unix)))]
mod internal_install_flow;
pub mod journey;
pub mod known_folders;
pub mod migration;
mod migration_durability;
#[cfg(target_os = "linux")]
pub mod migration_handoff;
#[cfg(target_os = "linux")]
pub(crate) mod migration_intent;
pub mod migration_journal;
mod migration_journal_log;
#[cfg(any(target_os = "linux", test))]
pub mod migration_native;
#[cfg(any(target_os = "linux", test))]
mod migration_native_journal;
mod migration_review;
pub mod original_admission;
#[cfg(target_os = "linux")]
pub mod service_sessions;
#[cfg(target_os = "linux")]
pub use migration_review::{
    GrantedHomeRoots, GrantedRoot, HomeRootSet, MountedRootChooser, MountedRootSelection, RootKind,
};
mod migration_source;
mod migration_store;
mod migration_worker;
pub mod model;
mod nofollow;
#[cfg(unix)]
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

#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub(crate) mod retained_source_io;

#[cfg(target_os = "linux")]
mod retained_source_relay;
#[cfg(target_os = "linux")]
mod retained_source_worker;

#[cfg(target_os = "linux")]
mod retained_source_session;

#[cfg(all(target_os = "linux", feature = "capture-worker"))]
pub(crate) mod retained_target_projection;

#[cfg(target_os = "linux")]
mod retained_projection_session;

#[cfg(target_os = "linux")]
mod retained_consumer_driver;
#[cfg(target_os = "linux")]
mod retained_consumer_lease;
#[cfg(target_os = "linux")]
mod retained_consumer_selection;
#[cfg(target_os = "linux")]
mod retained_open_dispatch;
#[cfg(target_os = "linux")]
mod retained_open_selection;

#[cfg(target_os = "linux")]
mod retained_history_read;
mod retained_selected_edit;
#[cfg(target_os = "linux")]
mod retained_selected_edit_session;
mod retained_selected_editor_history_read;
#[cfg(target_os = "linux")]
mod retained_selected_editor_owner;

#[cfg(all(test, target_os = "linux", feature = "native-capture-qualification"))]
pub(in crate::setup) mod qualification_registration;

mod desktop_journey;

mod journey_navigation;
