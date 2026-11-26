//! The default output (sink) and input (source): level, mute flag and identity, read
//! and changed through `wpctl`'s `@DEFAULT_AUDIO_SINK@` and `@DEFAULT_AUDIO_SOURCE@`.
//!
//! Every reader and writer goes through here — media keys, Quick Settings, Sound
//! settings, screen recording — so the command lines and their parsing exist once.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultNode {
    Sink,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeState {
    /// `0.0..=1.0`. The level the device is set to, whether or not muted.
    pub level: f32,
    pub muted: bool,
}

pub fn volume_state(node: DefaultNode) -> Result<VolumeState, String> {
    parse_volume_state(&capture(&["get-volume", node.token()])?)
}

pub fn set_volume(node: DefaultNode, pct: f32) -> Result<(), String> {
    run(&["set-volume", node.token(), &volume_arg(pct)])
}

pub fn set_mute(node: DefaultNode, muted: bool) -> Result<(), String> {
    run(&["set-mute", node.token(), if muted { "1" } else { "0" }])
}

pub fn toggle_mute(node: DefaultNode) -> Result<(), String> {
    run(&["set-mute", node.token(), "toggle"])
}

/// Whether the default sink is a real output. With no audio device
/// `WirePlumber` still offers a "Dummy Output" (`auto_null`) whose volume reads
/// and sets fine and does nothing, so a control over it has no device behind it.
pub fn has_output_device() -> bool {
    capture(&["inspect", DefaultNode::Sink.token()]).is_ok_and(|out| !is_dummy_sink(&out))
}

pub fn object_serial(node: DefaultNode) -> Result<String, String> {
    let inspect = capture(&["inspect", node.token()])?;
    parse_object_serial(&inspect).ok_or_else(|| "the default device has no object serial".into())
}

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
