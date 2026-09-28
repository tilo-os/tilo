//! Independent pixel oracle for the edited-content runtime proof.
//! Does not call Photos transforms or image-library rotation helpers.

pub(crate) fn verify_clockwise_rgba(
    source_width: u32,
    source_height: u32,
    source: &[u8],
    width: u32,
    height: u32,
    actual: &[u8],
) -> Result<(), String> {
    if source_width == 0 || source_height == 0 {
        return Err("empty source image".into());
    }
    if (width, height) != (source_height, source_width) {
        return Err(format!(
            "edited dimensions {width}x{height} are not clockwise source dimensions"
        ));
    }
    let sw = usize::try_from(source_width).map_err(|_| "source width exceeds address space")?;
    let sh = usize::try_from(source_height).map_err(|_| "source height exceeds address space")?;
    let byte_len = sw
        .checked_mul(sh)
        .and_then(|n| n.checked_mul(4))
        .ok_or("image byte count overflow")?;
    if source.len() != byte_len || actual.len() != byte_len {
        return Err("RGBA byte length does not match dimensions".into());
    }
    #[expect(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        reason = "source_y < sh and source_x < sw, so sh - 1 - source_y cannot underflow and both pixel offsets are at most sw * sh * 4 - 4; both buffers were checked to be exactly sw * sh * 4 bytes, computed without overflow"
    )]
    for source_y in 0..sh {
        for source_x in 0..sw {
            // A clockwise quarter-turn maps (x,y) to (H-1-y,x).
            let target_x = sh - 1 - source_y;
            let target_y = source_x;
            let from = (source_y * sw + source_x) * 4;
            let to = (target_y * sh + target_x) * 4;
            if source[from..from + 4] != actual[to..to + 4] {
                return Err(format!("edited RGBA differs at ({target_x},{target_y})"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::verify_clockwise_rgba;

    fn rgba(labels: &[u8]) -> Vec<u8> {
        labels
            .iter()
            .flat_map(|&n| [n, n.wrapping_add(10), n.wrapping_add(20), 255])
            .collect()
    }

    #[test]
    fn handoff_pixels_accept_only_the_asymmetric_clockwise_edit() {
        // Phase `asymmetric_clockwise_pixels_are_verified_independently`.
        {
            // Source: A B / C D / E F. Clockwise: E C A / F D B.
            let source = rgba(&[1, 2, 3, 4, 5, 6]);
            let clockwise = rgba(&[5, 3, 1, 6, 4, 2]);
            assert_eq!(
                verify_clockwise_rgba(2, 3, &source, 3, 2, &clockwise),
                Ok(())
            );
        }
        // Phase `original_counterclockwise_and_corrupt_pixels_are_rejected`.
        {
            let source = rgba(&[1, 2, 3, 4, 5, 6]);
            assert!(verify_clockwise_rgba(2, 3, &source, 2, 3, &source).is_err());
            assert!(
                verify_clockwise_rgba(2, 3, &source, 3, 2, &rgba(&[2, 4, 6, 1, 3, 5])).is_err()
            );
            let mut corrupt = rgba(&[5, 3, 1, 6, 4, 2]);
            corrupt[7] = 254;
            assert!(verify_clockwise_rgba(2, 3, &source, 3, 2, &corrupt).is_err());
            assert!(verify_clockwise_rgba(2, 3, &source, 3, 2, &corrupt[..23]).is_err());
        }
    }
}
