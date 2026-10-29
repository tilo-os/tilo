//! The default output (sink) and input (source): their level, mute flag and
//! identity, read and changed through `wpctl`'s `@DEFAULT_AUDIO_SINK@` /
//! `@DEFAULT_AUDIO_SOURCE@` handles.
//!
//! Every reader and writer of the default devices (the media keys, Quick
//! Settings, the Sound settings, screen recording) goes through here, so the
//! command lines and the parsing of what `wpctl` prints exist once.

/// One of the two default devices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultNode {
    /// The default output.
    Sink,
    /// The default input (the microphone).
    Source,
}

impl DefaultNode {
    fn token(self) -> &'static str {
        match self {
            Self::Sink => "@DEFAULT_AUDIO_SINK@",
            Self::Source => "@DEFAULT_AUDIO_SOURCE@",
        }
    }
}

/// A default device's level and mute flag.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    /// `0.0..=1.0`. The level the device is set to, whether or not muted.
    pub level: f32,
    pub muted: bool,
}

/// Read `node`'s level and mute flag together: a sink at 0.60 that is muted
/// is silent, and asking twice would race a media key pressed in between.
///
/// # Errors
///
/// When `wpctl` is missing, fails, or prints something unreadable.
pub fn volume_state(node: DefaultNode) -> Result<VolumeState, String> {
    parse_volume_state(&capture(&["get-volume", node.token()])?)
}

/// Set `node`'s level to `pct` percent, clamped to `0..=100`.
///
/// # Errors
///
/// When `wpctl` is missing or fails.
pub fn set_volume(node: DefaultNode, pct: f32) -> Result<(), String> {
    run(&["set-volume", node.token(), &volume_arg(pct)])
}

/// Mute or unmute `node`.
///
/// # Errors
///
/// When `wpctl` is missing or fails.
pub fn set_mute(node: DefaultNode, muted: bool) -> Result<(), String> {
    run(&["set-mute", node.token(), if muted { "1" } else { "0" }])
}

/// Toggle `node`'s mute flag.
///
/// # Errors
///
/// When `wpctl` is missing or fails.
pub fn toggle_mute(node: DefaultNode) -> Result<(), String> {
    run(&["set-mute", node.token(), "toggle"])
}

/// Whether the default sink is a real output. With no audio device
/// `WirePlumber` still offers a "Dummy Output" (`auto_null`) whose volume reads
/// and sets fine and does nothing, so a control over it has no device behind it.
pub fn has_output_device() -> bool {
    capture(&["inspect", DefaultNode::Sink.token()]).is_ok_and(|out| !is_dummy_sink(&out))
}

/// `node`'s `object.serial`: the handle a capture stream's `--target` takes.
///
/// # Errors
///
/// When `wpctl` is missing or fails, or the device has no serial.
pub fn object_serial(node: DefaultNode) -> Result<String, String> {
    let inspect = capture(&["inspect", node.token()])?;
    parse_object_serial(&inspect).ok_or_else(|| "the default device has no object serial".into())
}

/// The clamped `0.0..=1.0` fraction, formatted as `wpctl` expects it. The
/// fraction form avoids locale-dependent `%` handling.
fn volume_arg(pct: f32) -> String {
    let clamped = (pct.clamp(0.0, 100.0) / 100.0).clamp(0.0, 1.0);
    format!("{clamped:.2}")
}

fn is_dummy_sink(inspect: &str) -> bool {
    inspect.lines().any(|line| {
        let line = line.trim_start_matches(|c: char| c.is_whitespace() || c == '*');
        line.strip_prefix("node.name")
            .and_then(|rest| rest.trim_start().strip_prefix('='))
            .is_some_and(|value| value.trim().trim_matches('"') == "auto_null")
    })
}

fn parse_object_serial(inspect: &str) -> Option<String> {
    inspect.lines().find_map(|line| {
        let (_, value) = line.trim().split_once("object.serial =")?;
        let serial = value.trim().trim_matches('"');
        (!serial.is_empty() && serial.chars().all(|ch| ch.is_ascii_digit()))
            .then(|| serial.to_string())
    })
}

/// `Volume: 0.45` or `Volume: 0.45 [MUTED]`.
fn parse_volume_state(stdout: &str) -> Result<VolumeState, String> {
    let line = stdout.lines().next().ok_or("wpctl printed nothing")?;
    let after = line
        .split(':')
        .nth(1)
        .ok_or_else(|| format!("wpctl format: {line}"))?
        .trim();
    let num_str = after.split_whitespace().next().unwrap_or("");
    let level: f32 = num_str
        .parse()
        .map_err(|e| format!("wpctl volume parse: {e} ({num_str:?})"))?;
    Ok(VolumeState {
        level: level.clamp(0.0, 1.0),
        muted: line.contains("[MUTED]"),
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn capture(args: &[&str]) -> Result<String, String> {
    let out = bounded_command::run("wpctl", args, None, 1024 * 1024, super::CONTROL_TIMEOUT)
        .map_err(|error| format!("wpctl: {error}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(out.stderr_diagnostic())
    }
}

#[cfg(target_arch = "wasm32")]
fn capture(_: &[&str]) -> Result<String, String> {
    Err("there are no PipeWire devices on this target".into())
}

fn run(args: &[&str]) -> Result<(), String> {
    capture(args).map(drop)
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
    fn volume_lines_parse_level_and_mute_or_fail() {
        let plain = parse_volume_state("Volume: 0.45\n").unwrap();
        assert!((plain.level - 0.45).abs() < 1e-6);
        assert!(!plain.muted);
        // A muted device still reports the level it will return to.
        let muted = parse_volume_state("Volume: 0.30 [MUTED]\n").unwrap();
        assert!((muted.level - 0.30).abs() < 1e-6);
        assert!(muted.muted);
        assert!(parse_volume_state("not-a-line").is_err());
    }

    /// Out-of-range input is clamped before it reaches `wpctl`, which would
    /// otherwise reject it or read it as a different level.
    #[test]
    fn set_volume_clamps_out_of_range() {
        assert_eq!(volume_arg(150.0), "1.00");
        assert_eq!(volume_arg(-25.0), "0.00");
        assert_eq!(volume_arg(50.0), "0.50");
    }

    #[test]
    fn object_serial_is_digits_only() {
        let inspect = "id 52, type PipeWire:Interface:Node\n  * object.serial = \"314\"\n";
        assert_eq!(parse_object_serial(inspect).as_deref(), Some("314"));
        assert_eq!(parse_object_serial("object.serial = hacked"), None);
    }
}
