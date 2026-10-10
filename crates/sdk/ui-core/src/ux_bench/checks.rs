use super::{Axis, Check, ImageRegion};

pub fn gap(a: [f64; 4], b: [f64; 4], axis: Axis) -> f64 {
    match axis {
        Axis::X => b[0] - a[0] - a[2],
        Axis::Y => b[1] - a[1] - a[3],
    }
}
pub fn overlap_area(a: [f64; 4], b: [f64; 4]) -> f64 {
    ((a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0])).max(0.0)
        * ((a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1])).max(0.0)
}
/// Contrast of two explicitly selected opaque rendered pixels.
/// # Errors
/// Transparent samples lack a known composited background.
pub fn pixel_contrast(a: [u8; 4], b: [u8; 4]) -> Result<f64, String> {
    if a[3] != 255 || b[3] != 255 {
        return Err("transparent sample: composited background unknown".into());
    }
    let lum = |p: [u8; 4]| {
        let linear = |v| {
            let c = f64::from(v) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2])
    };
    let (a, b) = (lum(a), lum(b));
    Ok((a.max(b) + 0.05) / (a.min(b) + 0.05))
}
pub(super) fn validate(c: &Check, viewport: [u32; 2]) -> Result<(), String> {
    let range = |min: f64, max: f64| {
        if min.is_finite() && max.is_finite() && min <= max {
            Ok(())
        } else {
            Err("invalid check range".into())
        }
    };
    match c {
        Check::ImageUnchanged {
            region: ImageRegion::Pixels(region),
            ..
        } => {
            if region[2] == 0
                || region[3] == 0
                || region[0]
                    .checked_add(region[2])
                    .is_none_or(|v| v > viewport[0])
                || region[1]
                    .checked_add(region[3])
                    .is_none_or(|v| v > viewport[1])
            {
                Err("invalid stationary image region".into())
            } else {
                Ok(())
            }
        }

        Check::ImageShift {
            region: ImageRegion::Pixels(region),
            min,
            max,
            ..
        } => {
            range(*min, *max)?;
            if *min < -256.0
                || *max > 256.0
                || region[2] < 24
                || region[3] < 24
                || region[0]
                    .checked_add(region[2])
                    .is_none_or(|x| x > viewport[0])
                || region[1]
                    .checked_add(region[3])
                    .is_none_or(|y| y > viewport[1])
            {
                Err("invalid image-shift region or range".into())
            } else {
                Ok(())
            }
        }
        Check::ImageUnchanged {
            region: ImageRegion::Target { target, .. },
            ..
        }
        | Check::ImageShift {
            region: ImageRegion::Target { target, .. },
            ..
        } => {
            if target.trim().is_empty() {
                return Err("empty image target".into());
            }
            if let Check::ImageShift { min, max, .. } = c {
                range(*min, *max)?;
                if *min < -256.0 || *max > 256.0 {
                    return Err("invalid image-shift range".into());
                }
            }
            Ok(())
        }
        Check::Gap { min, max, .. } | Check::Moved { min, max, .. } => range(*min, *max),
        Check::Aligned { tolerance, .. } => range(0.0, *tolerance),
        Check::TargetSize { min, .. } => {
            range(0.0, min[0])?;
            range(0.0, min[1])
        }
        Check::Contrast {
            foreground,
            background,
            min,
            ..
        } => {
            range(1.0, *min)?;
            if *min > 21.0
                || [foreground, background]
                    .iter()
                    .any(|p| p[0] >= viewport[0] || p[1] >= viewport[1])
            {
                Err("contrast sample outside image or invalid ratio".into())
            } else {
                Ok(())
            }
        }
        Check::Semantic { expectation } => crate::headless::parse_spec(
            &serde_json::json!({"actions":[{"expect":expectation}]}).to_string(),
        )
        .map(|_| ()),
        _ => Ok(()),
    }
}

/// Estimate translation of visible content in a specified screenshot region.
/// Unchanged or weakly matching pixels are insufficient evidence of scrolling.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "u32 coordinates widened to i64 plus shifts bounded to 256 cannot overflow"
)]
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn image_shift(
    before: &image::RgbaImage,
    after: &image::RgbaImage,
    region: [u32; 4],
    axis: Axis,
) -> Result<(i32, f64, f64), String> {
    if before.dimensions() != after.dimensions() {
        return Err("capture dimensions differ".into());
    }
    let [left, top, width, height] = region;
    let end_x = left.checked_add(width).ok_or("region overflow")?;
    let end_y = top.checked_add(height).ok_or("region overflow")?;
    if width < 24 || height < 24 || end_x > before.width() || end_y > before.height() {
        return Err("invalid image-shift region".into());
    }
    let extent = match axis {
        Axis::X => width,
        Axis::Y => height,
    };
    let limit = i32::try_from((extent / 2).min(256)).map_err(|e| e.to_string())?;
    let error = |shift: i32| {
        let mut sum = 0.0_f64;
        let mut samples = 0.0_f64;
        for py in (top..end_y).step_by(4) {
            for px in (left..end_x).step_by(4) {
                let qx = i64::from(px)
                    + if matches!(axis, Axis::X) {
                        i64::from(shift)
                    } else {
                        0
                    };
                let qy = i64::from(py)
                    + if matches!(axis, Axis::Y) {
                        i64::from(shift)
                    } else {
                        0
                    };
                if qx < i64::from(left)
                    || qx >= i64::from(end_x)
                    || qy < i64::from(top)
                    || qy >= i64::from(end_y)
                {
                    continue;
                }
                let a = before.get_pixel(px, py).0;
                let (Ok(qx), Ok(qy)) = (u32::try_from(qx), u32::try_from(qy)) else {
                    continue;
                };
                let b = after.get_pixel(qx, qy).0;
                sum += f64::from(a[0].abs_diff(b[0]))
                    + f64::from(a[1].abs_diff(b[1]))
                    + f64::from(a[2].abs_diff(b[2]));
                samples += 3.0;
            }
        }
        sum / samples.max(1.0)
    };
    let zero = error(0);
    if zero < 0.25 {
        return Err("region did not visibly change; no scroll displacement established".into());
    }
    let mut best = (0, zero);
    for shift in limit.saturating_neg()..=limit {
        let score = error(shift);
        if score < best.1 {
            best = (shift, score);
        }
    }
    if best.0 == 0 || best.1 > zero * 0.45 {
        return Err(format!(
            "no reliable translation: best error {}, unshifted error {zero}",
            best.1
        ));
    }
    Ok((best.0, best.1, zero))
}
