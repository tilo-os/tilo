//! Native installation actors and portable contracts.
//! Frontends retain presentation state and issue requests to these actors.

pub mod cache;
pub mod media_model;
pub mod probe;
pub mod profile;
pub mod record;
pub mod staging;
pub mod startup_password;
pub mod write;

pub mod firmware;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod linux_parse;
pub mod model;
pub mod plan;
pub mod platform;
pub mod setup;

#[cfg(all(feature = "destination", target_os = "linux"))]
pub mod destination;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

/// Every start and size is aligned to this many bytes.
pub const ALIGN_BYTES: u64 = 1 << 20;
/// The smallest root installed beside another system.
pub const MIN_ALONGSIDE_BYTES: u64 = staging_handoff_model::sizes::MIN_ROOT_BYTES;
/// Tilo keeps several signed UKIs for rollback and repair, which a 100--512 MiB ESP cannot hold.
pub const MIN_ALONGSIDE_ESP_BYTES: u64 = staging_handoff_model::sizes::ESP_BYTES;
/// One boot partition, large enough for the kernel and initramfs of several entries.
pub const BOOT_BYTES: u64 = staging_handoff_model::sizes::BOOT_BYTES;
/// The least room Tilo takes on a disk, alongside or erased: ESP, boot and the smallest root.
pub const MIN_TARGET_BYTES: u64 = staging_handoff_model::sizes::MIN_TILO_BYTES;
/// GPT type of the staging ESP.
pub const ESP_TYPE: &str = staging_handoff_model::gpt::EFI_SYSTEM;
/// Firmware boot option description.
pub const BOOT_ENTRY_DESCRIPTION: &str = "Tilo installer";

/// A random RFC 4122 version-4 GUID in canonical lowercase form.
pub fn random_guid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// An operation, attempt or request id, as [`random_guid`] writes it.
pub use staging_handoff_model::is_operation_id;

/// Canonical lowercase GUID text (`8-4-4-4-12` hex digits).
pub fn is_guid(value: &str) -> bool {
    staging_handoff_model::is_guid(value) && !value.bytes().any(|byte| byte.is_ascii_uppercase())
}

/// Two GUIDs name the same object regardless of case or Windows braces.
pub fn same_guid(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim_matches(|c| c == '{' || c == '}')
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::{ESP_TYPE, is_guid, random_guid, same_guid, staging::STAGING_ROOT_TYPE};

    #[test]
    fn guids_are_canonical_random_and_compared_by_identity() {
        {
            let a = random_guid();
            let b = random_guid();
            assert!(is_guid(&a), "{a}");
            assert_ne!(a, b);
            assert_eq!(&a[14..15], "4");
            assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
        }

        {
            assert!(same_guid(
                "{C12A7328-F81F-11D2-BA4B-00A0C93EC93B}",
                ESP_TYPE
            ));
            assert!(!same_guid(ESP_TYPE, STAGING_ROOT_TYPE));
        }
    }
}
