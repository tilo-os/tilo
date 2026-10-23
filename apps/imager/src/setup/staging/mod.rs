//! Host-side staging for USB-free Full Tilo installation.
//!
//! Contract: `docs/architecture/usb-free-install.md`.
//! The unprivileged UI probes and plans; only the elevated helper
//! (`--tilo-staging-helper`) mutates disks or firmware, and it revalidates the
//! reviewed plan against a fresh probe before every mutation.

#[cfg(target_os = "linux")]
pub mod browser;
// Pure journal/ordering contract is shared with cross-platform records.
pub(super) mod btrfs_resize;
pub mod bundle;
#[cfg(feature = "installation")]
pub(crate) mod capacity_probe;
pub mod cli;
pub mod efi;
pub mod fat;
pub mod final_bootnext;
pub mod handoff;
pub mod helper;
#[cfg(target_os = "linux")]
pub mod linux;
// Pure parsers shared by the Linux adapter; compiled everywhere so their
// tests run on every development host.
pub mod linux_parse;
pub mod plan;
pub mod platform;
pub mod prepared_setup;
pub mod probe;
pub mod record;
#[cfg(any(windows, test))]
pub mod windows;

#[cfg(all(test, feature = "installation"))]
pub(crate) mod fake;

/// Every start and size is aligned to this many bytes.
pub use imager_backend::ALIGN_BYTES;
/// The installer's guided-alongside minimum: 2 GiB ESP + 1 GiB boot + 32 GiB root.
pub use imager_backend::MIN_TARGET_BYTES;
/// Default Tilo size offered on Review.
pub const DEFAULT_TARGET_BYTES: u64 = 64 << 30;
/// Conservative staging sizes until a verified bundle supplies its requirements.
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ESP_BYTES: u64 = 2 << 30;
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ROOT_BYTES: u64 = 40 << 30;
/// GPT type of the staging ESP.
pub use imager_backend::ESP_TYPE;
/// GPT type of the staging root: the Tilo staging root. The live installer UKI
/// boots the one partition of this type on its disk (`root=tilo-auto`, see
/// os/usr/libexec/tilo-root-discover); an installed UKI never considers it, so
/// the staging root beside a freshly installed Tilo root is never ambiguous.
pub const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
/// GPT partition names; informational only, never used as identity.
pub const STAGING_ESP_NAME: &str = "Tilo installer boot";
pub const STAGING_ROOT_NAME: &str = "Tilo installer";
/// Firmware boot option description.
pub use imager_backend::BOOT_ENTRY_DESCRIPTION;
/// Directory on the staging ESP holding the handoff and record copies.
pub const ESP_STATE_DIR: &str = "/EFI/tilo-staging";
/// Name of the record copy inside [`ESP_STATE_DIR`].
pub const ESP_RECORD_FILE: &str = "record.json";

pub use imager_backend::{is_guid, random_guid, same_guid};

#[cfg(test)]
mod tests {
    /// GUID generation and comparison.
    #[test]
    fn guids_are_canonical_random_and_compared_by_identity() {
        // Phase `random_guids_are_canonical_v4_and_distinct`.
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
