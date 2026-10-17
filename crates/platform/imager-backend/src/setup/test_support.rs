//! Protocol fixtures shared by native setup tests.

pub(crate) mod media;

/// A protocol-shaped declaration for tests that separately attest the consumer.
pub(crate) fn home_consumer() -> crate::home_admission::ConsumerCapability {
    crate::home_admission::ConsumerCapability {
        schema: 1,
        archive_protocol: crate::home_admission::ARCHIVE_PROTOCOL,
        seed_protocol: 1,
        firstboot_user_sha256: "1".repeat(64),
        seed_helper_sha256: "2".repeat(64),
        prepared_helper_sha256: "3".repeat(64),
        tar_sha256: "4".repeat(64),
    }
}
