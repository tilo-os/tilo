//! Audio sink control via `PipeWire`'s `wpctl`.

use crate::process::{run, run_capture};

/// Set the default audio sink volume to `pct` percent. `pct` is clamped
/// to `[0, 100]`.
pub fn set_volume(pct: f32) -> Result<(), String> {
    // wpctl takes a fraction like `0.50` or `50%`. We use the fraction
    // form to avoid locale-dependent `%` handling in shells.
    let arg = volume_arg(pct);
    run("wpctl", &["set-volume", "@DEFAULT_AUDIO_SINK@", &arg])
}

/// The clamped `0.0..=1.0` fraction, formatted as `wpctl` expects it.
///
/// Named rather than inlined so the clamping can be asserted on its own: the
/// shell-out around it succeeds, fails, or reports a missing binary depending
/// on whether the machine has `wpctl` and a live audio session, none of which
/// says anything about the arithmetic.
fn volume_arg(pct: f32) -> String {
    let clamped = (pct.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
    format!("{clamped:.2}")
}

/// Read the default audio sink volume, returned as a `0.0..=1.0`
/// fraction.
///
/// # Errors
///
/// Returns an error message if `wpctl` is not installed, the command fails, or its output
/// cannot be parsed.
pub fn get_volume() -> Result<f32, String> {
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
    parse_wpctl_volume(&out)
}

/// Whether the default sink is a real output. With no audio device
/// `WirePlumber` still offers a "Dummy Output" (`auto_null`) whose volume reads
/// and sets fine and does nothing, so a control over it has no device behind it.
pub fn has_output_device() -> bool {
    run_capture("wpctl", &["inspect", "@DEFAULT_AUDIO_SINK@"]).is_ok_and(|out| !is_dummy_sink(&out))
}

fn is_dummy_sink(inspect: &str) -> bool {
    inspect.lines().any(|line| {
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '*');
        line.strip_prefix("node.name")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| value.trim().trim_matches('"') == "auto_null")
    })
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
///
/// # Errors
///
/// Returns an error message if `wpctl` is not installed, either command fails, or the
/// read-back output cannot be parsed.
pub fn toggle_source_mute() -> Result<bool, String> {
    run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"])?;
    let out = run_capture("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"])?;
    Ok(parse_wpctl_volume_state(&out)?.muted)
}

/// Read the default source's level and mute flag.
///
/// # Errors
///
/// Returns an error message if `wpctl` is not installed, the command fails, or its output
/// cannot be parsed.
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
///
/// # Errors
///
/// Returns an error message if `wpctl` is not installed, the command fails, or its output
/// cannot be parsed.
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
    fn only_the_dummy_output_counts_as_no_device() {
        let dummy = "id 34, type PipeWire:Interface:Node\n    media.class = \"Audio/Sink\"\n  * node.description = \"Dummy Output\"\n  * node.name = \"auto_null\"\n";
        let real = "id 52, type PipeWire:Interface:Node\n  * node.description = \"Built-in Audio Analog Stereo\"\n  * node.name = \"alsa_output.pci-0000_00_1f.3.analog-stereo\"\n";
        assert!(is_dummy_sink(dummy));
        assert!(!is_dummy_sink(real));
        assert!(!is_dummy_sink(""));
    }

    #[test]
    fn wpctl_volume_lines_parse_level_and_mute_or_fail() {
        for (case, line, level, muted) in [
            ("parse_wpctl_volume_simple", "Volume: 0.45", 0.45, false),
            (
                "parse_wpctl_volume_muted_suffix",
                "Volume: 0.30 [MUTED]",
                0.30,
                true,
            ),
        ] {
            let v = parse_wpctl_volume(line).unwrap();
            assert!((v - level).abs() < 1e-6, "case {case}: level {v}");
            // The state parser reads level and mute together; a muted sink
            // still reports the level it will return to.
            let state = parse_wpctl_volume_state(&format!("{line}\n")).unwrap();
            assert!(
                (state.level - level).abs() < 1e-6,
                "case {case}: state level"
            );
            assert_eq!(state.muted, muted, "case {case}: muted");
        }
        // parse_wpctl_volume_garbage_is_err
        assert!(parse_wpctl_volume("not-a-line").is_err());
    }

    /// Out-of-range input is clamped before it reaches `wpctl`, which would
    /// otherwise reject it or read it as a different level.
    #[test]
    fn set_volume_clamps_out_of_range() {
        assert_eq!(volume_arg(150.0), "1.00", "above range clamps to full");
        assert_eq!(volume_arg(-25.0), "0.00", "below range clamps to silent");
        assert_eq!(volume_arg(50.0), "0.50", "in range converts to a fraction");
    }
}
