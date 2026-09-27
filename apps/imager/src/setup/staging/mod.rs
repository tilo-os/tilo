//! Host-side staging for USB-free Full Tilo installation.
//!
//! Contract: `docs/superpowers/specs/2026-09-22-usb-free-install-design.md`.
//! The unprivileged UI probes and plans; only the elevated helper
//! (`--tilo-staging-helper`) mutates disks or firmware, and it revalidates the
//! reviewed plan against a fresh probe before every mutation.

pub mod bundle;
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
pub mod probe;
pub mod record;
#[cfg(any(windows, test))]
pub mod windows;

#[cfg(test)]
pub(crate) mod fake;

/// Every start and size is aligned to this many bytes.
pub const ALIGN_BYTES: u64 = 1 << 20;
/// The installer's guided-alongside minimum: 2 GiB ESP + 1 GiB boot + 32 GiB root.
pub const MIN_TARGET_BYTES: u64 = 35 << 30;
/// Default Tilo size offered on Review.
pub const DEFAULT_TARGET_BYTES: u64 = 64 << 30;
/// GPT type of the staging ESP.
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// GPT type of the staging root: the Tilo staging root. The live installer UKI
/// boots the one partition of this type on its disk (`root=tilo-auto`, see
/// os/usr/libexec/tilo-root-discover); an installed UKI never considers it, so
/// the staging root beside a freshly installed Tilo root is never ambiguous.
pub const STAGING_ROOT_TYPE: &str = "d4c4daf6-f72c-43ee-b8e3-5ba32550311d";
/// GPT partition names; informational only, never used as identity.
pub const STAGING_ESP_NAME: &str = "Tilo installer boot";
pub const STAGING_ROOT_NAME: &str = "Tilo installer";
/// Firmware boot option description.
pub const BOOT_ENTRY_DESCRIPTION: &str = "Tilo installer";
/// Directory on the staging ESP holding the handoff and record copies.
pub const ESP_STATE_DIR: &str = "/EFI/tilo-staging";
/// Name of the record copy inside [`ESP_STATE_DIR`].
pub const ESP_RECORD_FILE: &str = "record.json";

/// A random RFC 4122 version-4 GUID in canonical lowercase form.
pub fn random_guid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Canonical lowercase GUID text (`8-4-4-4-12` hex digits).
pub fn is_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
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

        // Phase `guid_identity_ignores_case_and_braces`.
        {
            assert!(super::same_guid(
                "{C12A7328-F81F-11D2-BA4B-00A0C93EC93B}",
                super::ESP_TYPE
            ));
            assert!(!super::same_guid(super::ESP_TYPE, super::STAGING_ROOT_TYPE));
        }
    }
}
