//! Host-side staging for USB-free Full Tilo installation.
//!
//! Contract: `docs/superpowers/specs/2026-09-22-usb-free-install-design.md`.
//! The unprivileged UI probes and plans; only the elevated helper
//! (`--tilo-staging-helper`) mutates disks or firmware, and it revalidates the
//! reviewed plan against a fresh probe before every mutation.

pub mod efi;
pub mod handoff;
pub mod helper;
pub mod plan;
pub mod platform;
pub mod probe;
pub mod record;

/// Every start and size is aligned to this many bytes.
pub const ALIGN_BYTES: u64 = 1 << 20;
/// The installer's guided-alongside minimum: 2 GiB ESP + 32 GiB root.
pub const MIN_TARGET_BYTES: u64 = 34 << 30;
/// Default Tilo size offered on Review.
pub const DEFAULT_TARGET_BYTES: u64 = 64 << 30;
/// GPT type of the staging ESP.
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
/// GPT type of the staging root: generic Linux filesystem, deliberately not the
/// Tilo root type so installed-target discovery still sees exactly one root.
pub const STAGING_ROOT_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// GPT partition names; informational only, never used as identity.
pub const STAGING_ESP_NAME: &str = "Tilo installer boot";
pub const STAGING_ROOT_NAME: &str = "Tilo installer";
/// Firmware boot option description.
pub const BOOT_ENTRY_DESCRIPTION: &str = "Tilo installer";
/// Directory on the staging ESP holding the handoff and record copies.
pub const ESP_STATE_DIR: &str = "/EFI/tilo-staging";
