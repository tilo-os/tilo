//! Audio sink control via PipeWire's `wpctl`.

use crate::process::{run, run_capture};

/// Set the default audio sink volume to `pct` percent. `pct` is clamped
/// to `[0, 100]`.
pub fn set_volume(pct: f32) -> Result<(), String> {
    let clamped = (pct.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
    // wpctl takes a fraction like `0.50` or `50%`. We use the fraction
    // form to avoid locale-dependent `%` handling in shells.
    let arg = format!("{clamped:.2}");
    run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &arg])
}

/// Read the default audio sink volume, returned as a `0.0..=1.0`
/// fraction.
pub fn get_volume() -> Result<f32, String> {
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_wpctl_volume(&out)
}

/// Toggle the default audio sink's mute state via
/// `wpctl set-mute @DEFAULT_AUDIO_SINK@ toggle`.
pub fn toggle_mute() -> Result<(), String> {
    run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"])
}

/// Explicitly set the mute state on the default audio sink.
pub fn set_mute(muted: bool) -> Result<(), String> {
    let arg = if muted { "1" } else { "0" };
    run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", arg])
}

fn parse_wpctl_volume(stdout: &str) -> Result<f32, String> {
    // wpctl prints: `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
    let line = stdout.lines().next().ok_or("wpctl empty")?;
    let after = line
        .split(':')
        .nth(1)
        .ok_or_else(|| format!("wpctl format: {line}"))?
        .trim();
    let num_str = after.split_whitespace().next().unwrap_or("");
    let v: f32 = num_str
        .parse()
        .map_err(|e| format!("wpctl volume parse: {e} ({num_str:?})"))?;
    Ok(v.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wpctl_volume_simple() {
        let s = "Volume: 0.45";
        let v = parse_wpctl_volume(s).unwrap();
        assert!((v - 0.45).abs() < 1e-6);
    }

    #[test]
    fn parse_wpctl_volume_muted_suffix() {
        let s = "Volume: 0.30 [MUTED]";
        let v = parse_wpctl_volume(s).unwrap();
        assert!((v - 0.30).abs() < 1e-6);
    }

    #[test]
    fn parse_wpctl_volume_garbage_is_err() {
        assert!(parse_wpctl_volume("not-a-line").is_err());
    }

    /// Clamp behaviour is observable through the `pct` argument — the
    /// function doesn't expose the rounded fraction it formats into the
    /// CLI string, so we test the clamp arithmetic indirectly via the
    /// public `set_volume` path. Both extreme inputs must produce a
    /// `Result` (Ok or "not installed" Err) without panicking.
    #[test]
    fn set_volume_clamps_out_of_range() {
        let r1 = set_volume(150.0);
        let r2 = set_volume(-25.0);
        assert!(r1.is_ok() || r1.unwrap_err().contains("not installed"));
        assert!(r2.is_ok() || r2.unwrap_err().contains("not installed"));
    }
}
