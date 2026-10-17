//! Compatibility facade for backend-owned Home consumer admission.
pub use imager_backend::home_admission::*;

#[cfg(test)]
pub(in crate::setup) fn fixture() -> ConsumerCapability {
    ConsumerCapability {
        schema: 1,
        archive_protocol: ARCHIVE_PROTOCOL,
        seed_protocol: 1,
        firstboot_user_sha256: "1".repeat(64),
        seed_helper_sha256: "2".repeat(64),
        prepared_helper_sha256: "3".repeat(64),
        tar_sha256: "4".repeat(64),
    }
}
