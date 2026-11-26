//! Benchmark-only visual acknowledgement from the controlled scroll fixture.
//! The marker is 32 four-pixel cells at the page's physical top-left: magic,
//! signed wheel delta (big endian), then an XOR check byte. No page text is read.
pub(super) fn decode(rgba: &[u8], width: u32, height: u32) -> Option<i16> {
    if width < 128 || height < 4 {
        return None;
    }
    let stride = usize::try_from(width).ok()?.checked_mul(4)?;
    let size = stride.checked_mul(usize::try_from(height).ok()?)?;
    if rgba.len() != size {
        return None;
    }
    let row = rgba.chunks_exact(stride).nth(2)?.get(..512)?;
    let mut bytes = [0u8; 4];
    for (byte, cells) in bytes.iter_mut().zip(row.chunks_exact(128)) {
        for cell in cells.chunks_exact(16) {
            let [red, green, blue, alpha] = cell.get(8..12)? else {
                return None;
            };
            if *alpha != 255 {
                return None;
            }
            let channels = [*red, *green, *blue];
            let value = if channels.iter().all(|v| *v >= 250) {
                1
            } else if channels.iter().all(|v| *v <= 5) {
                0
            } else {
                return None;
            };
            *byte = (*byte << 1) | value;
        }
    }
    let [magic, high, low, check] = bytes;
    if magic != 0xb3 || check != magic ^ high ^ low {
        return None;
    }
    let delta = i16::from_be_bytes([high, low]);
    (delta != 0).then_some(delta)
}
