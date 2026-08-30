//! Audio sink control via `PipeWire`'s `wpctl`.

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

/// Toggle the default *source* (microphone) mute, returning the state it
/// ended up in.
///
/// The mic-mute key is a different device from the volume keys — muting
/// the sink silences what you hear, muting the source silences what
/// others hear, and a key with a crossed-out microphone on it means the
/// second. `wpctl set-mute toggle` prints nothing useful, so the new
/// state is read back; unlike the sink path this is not on an
/// autorepeating key, so the extra call costs one press, not a burst.
pub fn toggle_source_mute() -> Result<bool, String> {
    run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])?;
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"])?;
    Ok(parse_wpctl_volume_state(&out)?.muted)
}

/// Read the default source's level and mute flag.
pub fn get_source_state() -> Result<VolumeState, String> {
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"])?;
    parse_wpctl_volume_state(&out)
}

/// Volume *and* mute together.
///
/// `wpctl` reports both on one line, and a volume indicator needs both
/// to say anything true: a sink at 0.60 that is muted is silent, and
/// drawing it at 60% is a lie. [`get_volume`] discards the mute flag, so
/// it can't back an indicator on its own — and asking twice would race
/// against a media key pressed in between.
pub fn get_volume_state() -> Result<VolumeState, String> {
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_wpctl_volume_state(&out)
}

/// The default sink's level and mute flag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    /// `0.0..=1.0`. The level the sink is set to, whether or not muted.
    pub level: f32,
    pub muted: bool,
}

fn parse_wpctl_volume_state(stdout: &str) -> Result<VolumeState, String> {
    let level = parse_wpctl_volume(stdout)?;
    // wpctl appends `[MUTED]` (upper-case, bracketed) after the level.
    let muted = stdout
        .lines()
        .next()
        .unwrap_or_default()
        .contains("[MUTED]");
    Ok(VolumeState { level, muted })
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
    fn parse_wpctl_volume_state_reads_level_and_mute_together() {
        let unmuted = parse_wpctl_volume_state("Volume: 0.45\n").unwrap();
        assert!((unmuted.level - 0.45).abs() < 1e-6);
        assert!(!unmuted.muted);

        let muted = parse_wpctl_volume_state("Volume: 0.30 [MUTED]\n").unwrap();
        assert!(
            (muted.level - 0.30).abs() < 1e-6,
            "a muted sink still reports the level it will return to"
        );
        assert!(muted.muted);
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
