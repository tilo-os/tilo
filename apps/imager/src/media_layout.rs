//! Installer layout operations supplied by the native backend.
#[cfg(test)]
use imager_backend::media_layout::gpt;
pub use imager_backend::media_layout::{
    DATA_PARTITION_NAME, DATA_PARTITION_TYPE, MediaPartitionBinding, binding, provision,
};
#[cfg(any(test, feature = "installation"))]
use std::{fs::File, io};
#[cfg(all(test, feature = "native-capture-qualification"))]
mod native_capacity_tests;
#[cfg(test)]
pub(crate) mod tests;

#[cfg(any(test, feature = "installation"))]
pub(crate) use imager_backend::media_layout::PreparedLayout;

#[cfg(feature = "installation")]
pub(crate) fn prepare(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    stable_id: &str,
) -> io::Result<PreparedLayout> {
    imager_backend::media_layout::prepare(
        image,
        target,
        image_bytes,
        target_bytes,
        stable_id,
        crate::setup::portable_home_media::payload_capacity_for_free_bytes,
    )
}
#[cfg(test)]
pub(crate) fn prepare_expected(
    image: &File,
    target: &File,
    image_bytes: u64,
    target_bytes: u64,
    expected: &MediaPartitionBinding,
    reservation: u64,
) -> io::Result<PreparedLayout> {
    imager_backend::media_layout::prepare_expected(
        image,
        target,
        image_bytes,
        target_bytes,
        expected,
        reservation,
        crate::setup::portable_home_media::payload_capacity_for_free_bytes,
    )
}
