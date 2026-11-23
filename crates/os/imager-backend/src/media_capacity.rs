//! Checked capacity reservation for the existing migration GPT + ext4 route.
const MIB: u64 = 1024 * 1024;
pub fn required(image: u64, payload: u64, migration: bool, installable: bool) -> Option<u64> {
    if image == 0 {
        return None;
    }
    if !migration {
        return image.checked_add(if installable {
            crate::media_model::PROFILE_BLOCK_SIZE as u64
        } else {
            0
        });
    }
    // Payload is ciphertext: only a handful of files, not unpacked home inodes.
    // Ten percent covers ext4 metadata/reserved blocks; the fixed allowance
    // covers its journal on small filesystems. Actual free space is rechecked.
    let filesystem = payload
        .checked_add(payload.div_ceil(10))?
        .checked_add(64 * MIB)?
        .max(16 * MIB);
    // GPT parser bounds its backup array to 512KiB. One aligned MiB covers
    // that array, the 64KiB profile gap and backup header at the device tail.
    image
        .div_ceil(MIB)
        .checked_mul(MIB)?
        .checked_add(filesystem.div_ceil(MIB).checked_mul(MIB)?)?
        .checked_add(MIB)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_images_and_overflow_never_become_writable_capacity() {
        assert_eq!(required(0, 4096, true, true), None);
        assert_eq!(required(u64::MAX, 4096, true, true), None);
        assert_eq!(required(4096, u64::MAX, true, true), None);
        assert_eq!(required(4096, 0, false, false), Some(4096));
    }
}
