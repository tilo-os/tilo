//! Host-side staging for a USB-free Full Tilo install: the unprivileged UI probes and
//! plans, and only the elevated helper mutates disks or firmware, revalidating the plan
//! against a fresh probe first.

#[cfg(feature = "installation")]
pub(crate) mod capacity_probe;
pub mod cli;
pub mod handoff;
pub mod helper;
pub mod probe;
pub mod record;

#[cfg(all(test, feature = "installation"))]
pub(crate) mod fake;

pub const DEFAULT_TARGET_BYTES: u64 = 64 << 30;
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ESP_BYTES: u64 = 2 << 30;
#[cfg(feature = "installation")]
pub(crate) const PROVISIONAL_STAGING_ROOT_BYTES: u64 = 40 << 30;
pub const STAGING_ESP_NAME: &str = "Tilo installer boot";
pub const STAGING_ROOT_NAME: &str = "Tilo installer";
pub const ESP_STATE_DIR: &str = "/EFI/tilo-staging";
pub const ESP_RECORD_FILE: &str = "record.json";
