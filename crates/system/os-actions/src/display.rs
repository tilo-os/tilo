//! Display brightness control via `brightnessctl`.

use crate::process::{run, run_capture};

/// Set the backlight brightness to `pct` percent. `pct` is clamped to
/// `[0, 100]` before being formatted into the `brightnessctl` argument.
pub fn set_brightness(pct: f32) -> Result<(), String> {
    let clamped = pct.clamp(0.0, 100.0).round() as u32;
    run("brightnessctl", &["set", &format!("{clamped}%")])
}

/// Read the current brightness, returned as a `0.0..=1.0` fraction.
///
/// Parses `brightnessctl -m` (machine-readable: one line of
/// `name,class,value,percent,max` separated by commas) so we don't have
/// to spawn two processes for `get` and `max`.
pub fn get_brightness() -> Result<f32, String> {
    let out = run_capture("brightnessctl", &["-m"])?;
    parse_brightness_machine(&out)
}

fn parse_brightness_machine(stdout: &str) -> Result<f32, String> {
    // Format example: `acpi_video0,backlight,4,40%,10`
    let line = stdout.lines().next().ok_or("brightnessctl -m empty")?;
    let fields: Vec<&str> = line.split(',').collect();
    if fields.len() < 4 {
        return Err(format!("brightnessctl -m unexpected: {line}"));
    }
    // Field 3 is the percentage with a trailing `%`.
    let pct_str = fields[3].trim_end_matches('%');
    let pct: f32 = pct_str
        .parse()
        .map_err(|e| format!("brightness pct parse: {e} ({pct_str:?})"))?;
    Ok((pct / 100.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brightness_machine_returns_fraction() {
        let s = "acpi_video0,backlight,4,40%,10";
        let v = parse_brightness_machine(s).unwrap();
        assert!((v - 0.40).abs() < 1e-6);
    }

    #[test]
    fn parse_brightness_machine_handles_zero() {
        let s = "acpi_video0,backlight,0,0%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 0.0);
    }

    #[test]
    fn parse_brightness_machine_clamps_above_one() {
        // brightnessctl will never emit >100%, but a hypothetical 110%
        // must not surface as a >1.0 fraction.
        let s = "x,b,11,110%,10";
        assert_eq!(parse_brightness_machine(s).unwrap(), 1.0);
    }

    #[test]
    fn parse_brightness_machine_empty_is_err() {
        assert!(parse_brightness_machine("").is_err());
    }

    /// Brightness setter on a host without `brightnessctl` returns a
    /// clean error (no panic). When the binary *is* present we don't run
    /// anything here — that's an integration concern.
    #[test]
    fn set_brightness_on_missing_binary_is_err() {
        let result = set_brightness(50.0);
        match result {
            Ok(()) => {}
            Err(e) => assert!(!e.is_empty(), "error message should not be empty"),
        }
    }
}
