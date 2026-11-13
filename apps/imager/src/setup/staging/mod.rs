//! Host-side staging for a USB-free Full Tilo install: the unprivileged UI probes and
//! plans, and only the elevated helper mutates disks or firmware, revalidating the plan
//! against a fresh probe first.

#[cfg(target_os = "linux")]
pub mod browser;
pub mod bundle;
#[cfg(feature = "installation")]
pub(crate) mod capacity_probe;
pub mod cli;
pub mod handoff;
pub mod helper;
pub mod plan;
pub mod prepared_setup;
pub mod probe;
pub mod record;

#[cfg(all(test, feature = "installation"))]
pub(crate) mod fake;

pub use imager_backend::ALIGN_BYTES;
pub use imager_backend::MIN_TARGET_BYTES;
pub const DEFAULT_TARGET_BYTES: u64 = 64 << 30;
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ESP_BYTES: u64 = 2 << 30;
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ROOT_BYTES: u64 = 40 << 30;
pub use imager_backend::ESP_TYPE;
pub const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
pub const STAGING_ESP_NAME: &str = "Tilo installer boot";
pub const STAGING_ROOT_NAME: &str = "Tilo installer";
pub use imager_backend::BOOT_ENTRY_DESCRIPTION;
pub const ESP_STATE_DIR: &str = "/EFI/tilo-staging";
pub const ESP_RECORD_FILE: &str = "record.json";

pub use imager_backend::{is_guid, random_guid, same_guid};

#[cfg(test)]
mod tests {
    #[test]
    fn guids_are_canonical_random_and_compared_by_identity() {
        {
            let a = super::random_guid();
            let b = super::random_guid();
            assert!(super::is_guid(&a), "{a}");
            assert_ne!(a, b);
            assert_eq!(&a[14..15], "4");
            assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
        }

        {
            assert!(super::same_guid(
                "{C12A7328-F81F-11D2-BA4B-00A0C93EC93B}",
                super::ESP_TYPE
            ));
            assert!(!super::same_guid(super::ESP_TYPE, super::STAGING_ROOT_TYPE));
        }
    }
}
