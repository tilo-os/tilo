//! Native setup custody and continuation actors shared by installer frontends.
#[cfg(target_os = "linux")]
pub use crate::migration_review::{GrantedHomeRoots, GrantedRoot, HomeRootSet, RootKind};
pub use crate::{
    account_admission, account_transfer, applications_admission, destination_security, folders,
    home_admission, home_transfer, migration, migration_durability, migration_journal,
    migration_journal_log, migration_native, migration_review, migration_store, migration_worker,
    nofollow, original_admission,
};

#[cfg(unix)]
pub mod browser_profiles;
#[cfg(target_os = "linux")]
pub mod browser_transfer;
#[cfg(target_os = "linux")]
pub mod browser_transfer_helper;
#[cfg(any(test, feature = "test-support"))]
#[cfg(target_os = "linux")]
pub mod discovery_fixture;
pub mod flatpak_transfer;
#[cfg(target_os = "linux")]
pub mod home_capture;
#[cfg(target_os = "linux")]
pub mod home_capture_helper;
#[cfg(target_os = "linux")]
pub mod installed_copy;
#[cfg(target_os = "linux")]
pub mod installed_retained_actor;
pub mod inventory_storage;
pub mod live_selection;
#[cfg(target_os = "linux")]
pub mod media_setup;
pub mod migration_handoff;
pub mod migration_intent;
pub mod model;
pub mod native_host;
#[cfg(target_os = "linux")]
pub mod portable_home_helper;
#[cfg(target_os = "linux")]
pub mod portable_home_import;
#[cfg(target_os = "linux")]
pub mod portable_home_media;
#[cfg(target_os = "linux")]
pub mod portable_transfer;
pub use crate::prepared_identity;
#[cfg(target_os = "linux")]
pub mod prepared_setup;
#[cfg(target_os = "linux")]
pub mod prepared_setup_helper;
#[cfg(all(test, feature = "native-capture-qualification"))]
#[cfg(target_os = "linux")]
mod qualification_frontend;
#[cfg(all(
    any(test, feature = "test-support"),
    feature = "native-capture-qualification"
))]
#[cfg(target_os = "linux")]
pub mod qualification_registration;
#[cfg(target_os = "linux")]
pub mod read_media;
#[cfg(target_os = "linux")]
pub mod retained_consumer_driver;
#[cfg(target_os = "linux")]
pub mod retained_consumer_lease;
#[cfg(target_os = "linux")]
pub mod retained_consumer_selection;
#[cfg(target_os = "linux")]
pub mod retained_history_read;
#[cfg(target_os = "linux")]
pub mod retained_open_dispatch;
#[cfg(target_os = "linux")]
pub mod retained_open_selection;
#[cfg(target_os = "linux")]
pub mod retained_projection_session;
#[cfg(target_os = "linux")]
pub mod retained_selected_edit;
#[cfg(target_os = "linux")]
pub mod retained_selected_edit_session;
#[cfg(target_os = "linux")]
pub mod retained_selected_editor_history_read;
#[cfg(target_os = "linux")]
pub mod retained_selected_editor_owner;
#[cfg(target_os = "linux")]
pub mod retained_source_relay;
#[cfg(target_os = "linux")]
pub mod retained_source_worker;
pub mod service_sessions;
#[cfg(target_os = "linux")]
pub mod source_geometry;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod test_support;
#[cfg(target_os = "linux")]
pub mod user_transfer;

/// Expectations retained across read-only admission and the privileged writer.
/// They are not authority; the receiving actor independently validates them.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OfflineLayout {
    pub binding: crate::media_layout::MediaPartitionBinding,
    pub reserved_bytes: u64,
    pub setup: crate::imager_model::PreparedSetupExpectation,
    pub source_plans: Vec<Vec<u8>>,
    pub target_number: u64,
}

#[cfg(target_os = "linux")]
pub mod staging {
    pub use crate::{ALIGN_BYTES, MIN_TARGET_BYTES, is_guid, random_guid, same_guid};
    pub use crate::{efi, linux, linux_parse, plan, platform};
    pub(crate) mod prepared_setup {
        /// The elevation launcher's request-owner hint, never caller authority.
        /// RootSession independently admits the live peer's kernel credentials.
        pub(crate) fn caller_uid() -> std::io::Result<u32> {
            let effective = rustix::process::geteuid().as_raw();
            if effective != 0 {
                return Ok(effective);
            }
            let value = ["PKEXEC_UID", "SUDO_UID"]
                .iter()
                .find_map(|key| std::env::var(key).ok())
                .ok_or_else(|| std::io::Error::other("Missing elevation caller UID"))?;
            let uid = value
                .parse::<u32>()
                .map_err(|_| std::io::Error::other("Invalid elevation caller UID"))?;
            if uid == 0 {
                return Err(std::io::ErrorKind::PermissionDenied.into());
            }
            Ok(uid)
        }
    }
}
