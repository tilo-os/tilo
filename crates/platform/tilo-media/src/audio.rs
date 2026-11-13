//! The audio graph client: enumerate the `PipeWire`/`WirePlumber` node graph,
//! move one application's stream between output devices, set per-stream volume and
//! mute, switch the default device, and diff two enumerations to react to hotplug.
//!
//! Enumeration reads `pw-dump`, routing writes `WirePlumber`'s `target.object`
//! metadata through `pw-metadata`, and per-node volume, mute and the default device
//! go through `wpctl`: the daemon's tools rather than a link to `libpipewire-0.3`,
//! so the crate builds without `PipeWire` headers. Setting `target.object` on a
//! stream's id to a device's `object.serial` relinks the stream, and deleting the
//! key returns it to the default device; `linking.allow-moving-streams` gates it.

#[cfg(not(target_arch = "wasm32"))]
use std::process::Command;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use serde_json::Value;

pub mod capture;
pub mod default_node;
#[cfg(not(target_arch = "wasm32"))]
pub mod group_tap;
#[cfg(not(target_arch = "wasm32"))]
pub mod record;

#[cfg(not(target_arch = "wasm32"))]
const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);

// `node.name` of the private sink a speaker group plays from while it is this
// device's output. It is how a set is played, never an output of its own: Sound
// lists the set instead.
pub const SPEAKER_GROUP_SINK: &str = "tilo-speaker-group";

// `node.name` of the software sink a session without sound hardware plays through.
// It carries no device's name, so the surfaces that list outputs name it
// themselves.
pub const SESSION_SINK: &str = "tilo-sink";

#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    pub id: u32,
    pub serial: u32,
    pub name: String,
    pub description: String,
    pub is_default: bool,
    pub volume: Option<f32>,
    pub muted: Option<bool>,
    // From Props.channelVolumes; balance and channel-test controls must not appear
    // below two.
    pub channels: usize,
    // The active Bluetooth codec, when the `PipeWire` `BlueZ` node advertises it.
    // Informational: codec selection is exposed only through available profiles.
    pub codec: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub available: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioHardwareDevice {
    pub id: u32,
    pub description: String,
    pub active_profile: Option<u32>,
    pub profiles: Vec<DeviceProfile>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AudioStream {
    pub id: u32,
    pub serial: u32,
    pub app: String,
    pub media_name: Option<String>,
    pub playback: bool,
    pub target_id: Option<u32>,
    pub volume: Option<f32>,
    pub muted: Option<bool>,
}

impl AudioStream {
    /// A one-line label for the UI: `"App — media"` when a media name is known,
    /// otherwise just the app name.
    pub fn label(&self) -> String {
        match &self.media_name {
            Some(m) if !m.is_empty() => format!("{} — {}", self.app, m),
            _ => self.app.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioGraph {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub playback_streams: Vec<AudioStream>,
    pub record_streams: Vec<AudioStream>,
    pub hardware: Vec<AudioHardwareDevice>,
}

impl AudioGraph {
    pub fn default_sink(&self) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.is_default)
    }
    pub fn default_source(&self) -> Option<&AudioDevice> {
        self.sources.iter().find(|d| d.is_default)
    }
    pub fn sink_by_id(&self, id: u32) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.id == id)
    }
    pub fn sink_by_description(&self, desc: &str) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.description == desc)
    }

    /// A cheap fingerprint of the graph's topology — devices by name, streams by id
    /// and app — deliberately excluding volume and mute, so a volume drift is not a
    /// structural change. Equal signatures mean the UI need not rebuild; a different
    /// one means a device or app appeared or went.
    pub fn topology_signature(&self) -> String {
        let dev = |ds: &[AudioDevice]| {
            ds.iter()
                .map(|d| d.name.clone())
                .collect::<Vec<_>>()
                .join(",")
        };
        let st = |ss: &[AudioStream]| {
            ss.iter()
                .map(|s| format!("{}:{}", s.id, s.app))
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "sink[{}]|src[{}]|hw[{}]|pb[{}]|rec[{}]",
            dev(&self.sinks),
            dev(&self.sources),
            self.hardware
                .iter()
                .map(|d| format!("{}:{}", d.id, d.profiles.len()))
                .collect::<Vec<_>>()
                .join(","),
            st(&self.playback_streams),
            st(&self.record_streams),
        )
    }

    pub fn diff(old: &AudioGraph, new: &AudioGraph) -> GraphDelta {
        let by_name = |ds: &[AudioDevice]| ds.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        let added = |old: &[AudioDevice], new: &[AudioDevice]| {
            let have = by_name(old);
            new.iter()
                .filter(|d| !have.contains(&d.name))
                .cloned()
                .collect::<Vec<_>>()
        };
        GraphDelta {
            added_sinks: added(&old.sinks, &new.sinks),
            removed_sinks: added(&new.sinks, &old.sinks),
            added_sources: added(&old.sources, &new.sources),
            removed_sources: added(&new.sources, &old.sources),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphDelta {
    pub added_sinks: Vec<AudioDevice>,
    pub removed_sinks: Vec<AudioDevice>,
    pub added_sources: Vec<AudioDevice>,
    pub removed_sources: Vec<AudioDevice>,
}

impl GraphDelta {
    pub fn is_empty(&self) -> bool {
        self.added_sinks.is_empty()
            && self.removed_sinks.is_empty()
            && self.added_sources.is_empty()
            && self.removed_sources.is_empty()
    }
}

fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        .and_then(|n| u32::try_from(n).ok())
}

/// `wpctl` shows the cube root of `PipeWire`'s raw linear channel volume; invert
/// that so an enumerated `channelVolumes` matches the slider position a user set.
/// `wpctl set-volume` takes the perceptual value back, so the round trip is exact.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a cube-root perceptual volume is a small, near-1.0 ratio; f32's \
              range is never at risk"
)]
fn perceptual_from_channel_volumes(cv: &[f64]) -> Option<f32> {
    cv.iter()
        .copied()
        .reduce(f64::max)
        .map(|v| (v.max(0.0)).cbrt() as f32)
}

fn node_volume_mute(obj: &Value) -> (Option<f32>, Option<bool>) {
    let props = obj
        .get("info")
        .and_then(|i| i.get("params"))
        .and_then(|p| p.get("Props"))
        .and_then(|p| p.as_array());
    let Some(arr) = props else {
        return (None, None);
    };
    for entry in arr {
        if let Some(cv) = entry.get("channelVolumes").and_then(|c| c.as_array()) {
            let cv: Vec<f64> = cv.iter().filter_map(serde_json::Value::as_f64).collect();
            let vol = perceptual_from_channel_volumes(&cv);
            let mute = entry.get("mute").and_then(serde_json::Value::as_bool);
            return (vol, mute);
        }
    }
    (None, None)
}

fn node_channel_count(obj: &Value) -> usize {
    obj.get("info")
        .and_then(|i| i.get("params"))
        .and_then(|p| p.get("Props"))
        .and_then(Value::as_array)
        .and_then(|a| {
            a.iter()
                .find_map(|e| e.get("channelVolumes").and_then(Value::as_array))
        })
        .map_or(0, Vec::len)
}

/// Parse a `pw-dump` JSON document into an [`AudioGraph`]: the default-device
/// metadata, the `Link` objects (where each stream is routed) and the audio
/// `Node`s, ignoring video, MIDI, clients, ports and factories. An empty or
/// malformed document yields an empty graph.
pub fn parse_pw_dump(json: &str) -> AudioGraph {
    let root: Value = serde_json::from_str(json).unwrap_or(Value::Null);
    let objs = root.as_array().cloned().unwrap_or_default();
    let (default_sink_name, default_source_name) = default_device_names(&objs);
    let links = collect_links(&objs);
    build_graph(
        &objs,
        &links,
        default_sink_name.as_deref(),
        default_source_name.as_deref(),
    )
}

fn default_device_names(objs: &[Value]) -> (Option<String>, Option<String>) {
    let mut default_sink_name: Option<String> = None;
    let mut default_source_name: Option<String> = None;
    for o in objs {
        let Some(md) = o.get("metadata").and_then(|m| m.as_array()) else {
            continue;
        };
        for e in md {
            let key = e.get("key").and_then(|k| k.as_str()).unwrap_or("");
            let name = e
                .get("value")
                .and_then(|v| v.get("name"))
                .and_then(|n| n.as_str())
                .map(str::to_string);
            match key {
                "default.audio.sink" => default_sink_name = name,
                "default.audio.source" => default_source_name = name,
                _ => {}
            }
        }
    }
    (default_sink_name, default_source_name)
}

fn collect_links(objs: &[Value]) -> Vec<(u32, u32)> {
    let mut links: Vec<(u32, u32)> = Vec::new();
    for o in objs {
        let is_link = o
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.ends_with("Link"));
        if !is_link {
            continue;
        }
        let p = o.get("info").and_then(|i| i.get("props"));
        let out = p.and_then(|p| p.get("link.output.node")).and_then(as_u32);
        let inp = p.and_then(|p| p.get("link.input.node")).and_then(as_u32);
        if let (Some(out), Some(inp)) = (out, inp) {
            links.push((out, inp));
        }
    }
    links
}

fn build_graph(
    objs: &[Value],
    links: &[(u32, u32)],
    default_sink_name: Option<&str>,
    default_source_name: Option<&str>,
) -> AudioGraph {
    let mut graph = AudioGraph::default();
    for o in objs {
        let is_device = o
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.ends_with("Device"));
        if is_device {
            if let Some(hw) = parse_hardware_device(o) {
                graph.hardware.push(hw);
            }
            continue;
        }
        let is_node = o
            .get("type")
            .and_then(|t| t.as_str())
            .is_some_and(|t| t.ends_with("Node"));
        if !is_node {
            continue;
        }
        push_node(o, links, default_sink_name, default_source_name, &mut graph);
    }
    graph
}

fn parse_hardware_device(o: &Value) -> Option<AudioHardwareDevice> {
    let id = o.get("id").and_then(as_u32)?;
    let props = o.get("info").and_then(|i| i.get("props"));
    let description = props
        .and_then(|p| p.get("device.description"))
        .and_then(Value::as_str)
        .or_else(|| {
            props
                .and_then(|p| p.get("device.name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("Audio device")
        .to_string();
    let params = o.get("info").and_then(|i| i.get("params"));
    let active_profile = params
        .and_then(|p| p.get("Profile"))
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(|p| p.get("index"))
        .and_then(as_u32);
    let profiles = params
        .and_then(|p| p.get("EnumProfile"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let index = p.get("index").and_then(as_u32)?;
            let name = p
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let description = p
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or(&name)
                .to_string();
            let available = !matches!(p.get("available").and_then(Value::as_str), Some("no"));
            Some(DeviceProfile {
                index,
                name,
                description,
                available,
            })
        })
        .collect();
    Some(AudioHardwareDevice {
        id,
        description,
        active_profile,
        profiles,
    })
}

pub const ECHO_CANCEL_NODE_PREFIX: &str = "tilo_echo_cancel_";

pub const ECHO_CANCEL_SOURCE_NAME: &str = "tilo_echo_cancel_source";

fn push_node(
    o: &Value,
    links: &[(u32, u32)],
    default_sink_name: Option<&str>,
    default_source_name: Option<&str>,
    graph: &mut AudioGraph,
) {
    let Some(id) = o.get("id").and_then(as_u32) else {
        return;
    };
    let props = o.get("info").and_then(|i| i.get("props"));
    let getp = |k: &str| props.and_then(|p| p.get(k));
    let media_class = getp("media.class").and_then(|v| v.as_str()).unwrap_or("");
    // Serial is a `PipeWire` object identity; a reusable node id is not a
    // substitute. Values above `u32` are omitted.
    let Some(serial) = getp("object.serial").and_then(as_u32) else {
        return;
    };
    let name = getp("node.name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // The echo-cancelled microphone and its internal streams are a session
    // transform over the real devices, not a device or an app: listing them would
    // offer a microphone that records itself as the default.
    if name.starts_with(ECHO_CANCEL_NODE_PREFIX) {
        return;
    }
    let (volume, muted) = node_volume_mute(o);

    match media_class {
        "Audio/Sink" | "Audio/Source" => {
            let description = ["node.nick", "node.description"]
                .iter()
                .find_map(|key| {
                    getp(key)
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                })
                .unwrap_or(&name)
                .to_string();
            let default_name = if media_class == "Audio/Sink" {
                default_sink_name
            } else {
                default_source_name
            };
            let is_default = default_name == Some(name.as_str());
            let dev = AudioDevice {
                id,
                serial,
                name,
                description,
                is_default,
                volume,
                muted,
                channels: node_channel_count(o),
                codec: getp("api.bluez5.codec")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            };
            if media_class == "Audio/Sink" {
                graph.sinks.push(dev);
            } else {
                graph.sources.push(dev);
            }
        }
        "Stream/Output/Audio" | "Stream/Input/Audio" => {
            let playback = media_class == "Stream/Output/Audio";
            let app = getp("application.name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(&name)
                .to_string();
            let media_name = getp("media.name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            let target_id = if playback {
                links
                    .iter()
                    .find(|(out, _)| *out == id)
                    .map(|(_, inp)| *inp)
            } else {
                links
                    .iter()
                    .find(|(_, inp)| *inp == id)
                    .map(|(out, _)| *out)
            };
            let stream = AudioStream {
                id,
                serial,
                app,
                media_name,
                playback,
                target_id,
                volume,
                muted,
            };
            if playback {
                graph.playback_streams.push(stream);
            } else {
                graph.record_streams.push(stream);
            }
        }
        _ => {}
    }
}

/// `wpctl set-default <id>`. The default metadata records the device's
/// `node.name`, so a stream following the default follows the switch.
pub fn wpctl_set_default_args(node_id: u32) -> Vec<String> {
    vec!["set-default".into(), node_id.to_string()]
}

/// `pw-metadata -n default <stream-id> target.object <serial>`: `WirePlumber` sees
/// the metadata and relinks the stream, so this is the real graph edit.
pub fn pw_metadata_route_args(stream_id: u32, target_serial: u32) -> Vec<String> {
    vec![
        "-n".into(),
        "default".into(),
        stream_id.to_string(),
        "target.object".into(),
        target_serial.to_string(),
    ]
}

/// `pw-metadata -n default -d <stream-id> target.object`: clear a stream's explicit
/// target so it follows the default device again.
pub fn pw_metadata_unroute_args(stream_id: u32) -> Vec<String> {
    vec![
        "-n".into(),
        "default".into(),
        "-d".into(),
        stream_id.to_string(),
        "target.object".into(),
    ]
}

pub fn wpctl_set_volume_args(node_id: u32, perceptual: f32) -> Vec<String> {
    let frac = perceptual.clamp(0.0, 1.5); // wpctl allows >1.0 (boost); cap sanely
    vec![
        "set-volume".into(),
        node_id.to_string(),
        format!("{frac:.2}"),
    ]
}

pub fn wpctl_set_mute_args(node_id: u32, muted: bool) -> Vec<String> {
    vec![
        "set-mute".into(),
        node_id.to_string(),
        if muted { "1".into() } else { "0".into() },
    ]
}

pub fn wpctl_set_profile_args(device_id: u32, profile_index: u32) -> Vec<String> {
    vec![
        "set-profile".into(),
        device_id.to_string(),
        profile_index.to_string(),
    ]
}

/// Side-effect interface for editing the audio graph. Production
/// ([`RealGraphControl`]) shells out to `wpctl` and `pw-metadata`; tests inject a
/// recorder, so the settings pane's dispatch can be asserted without a daemon.
pub trait GraphControl: Send + Sync {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String>;
    fn set_default_source(&self, node_id: u32) -> Result<(), String>;
    fn route_stream(&self, stream_id: u32, target_serial: u32) -> Result<(), String>;
    fn unroute_stream(&self, stream_id: u32) -> Result<(), String>;
    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String>;
    fn set_node_mute(&self, node_id: u32, muted: bool) -> Result<(), String>;
    fn set_device_profile(&self, device_id: u32, profile_index: u32) -> Result<(), String>;
}

#[cfg(not(target_arch = "wasm32"))]
fn run(bin: &str, args: &[String]) -> Result<(), String> {
    let mut command = Command::new(bin);
    command.args(args);
    let out = bounded_command::run_prepared(command, None, 64 * 1024, CONTROL_TIMEOUT)
        .map_err(|e| format!("{bin} control failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(out.stderr_diagnostic())
    }
}

#[cfg(target_arch = "wasm32")]
fn run(_: &str, _: &[String]) -> Result<(), String> {
    Err("PipeWire audio graph control is unsupported on this platform".to_owned())
}

/// `pw-dump --monitor` in a process tree of its own: each change to the graph as
/// one JSON array on the stdout [`Self::start`] hands back. Dropping it kills and
/// reaps the tree, so a reader that goes away leaves no `pw-dump` behind.
pub struct GraphMonitorProcess {
    child: std::process::Child,
}

impl GraphMonitorProcess {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start() -> Option<(Self, std::process::ChildStdout)> {
        use std::process::Stdio;
        let mut command = Command::new("pw-dump");
        command
            .arg("--monitor")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        bounded_command::own_process_tree(&mut command);
        let mut child = command.spawn().ok()?;
        let Some(stdout) = child.stdout.take() else {
            bounded_command::kill_tree(&mut child);
            let _ = child.wait();
            return None;
        };
        Some((Self { child }, stdout))
    }

    #[cfg(target_arch = "wasm32")]
    pub fn start() -> Option<(Self, std::process::ChildStdout)> {
        None
    }
}

impl Drop for GraphMonitorProcess {
    fn drop(&mut self) {
        #[cfg(not(target_arch = "wasm32"))]
        bounded_command::kill_tree(&mut self.child);
        let _ = self.child.wait();
    }
}

pub struct RealGraphControl;

impl GraphControl for RealGraphControl {
    fn set_default_sink(&self, node_id: u32) -> Result<(), String> {
        run("wpctl", &wpctl_set_default_args(node_id))
    }
    fn set_default_source(&self, node_id: u32) -> Result<(), String> {
        run("wpctl", &wpctl_set_default_args(node_id))
    }
    fn route_stream(&self, stream_id: u32, target_serial: u32) -> Result<(), String> {
        run(
            "pw-metadata",
            &pw_metadata_route_args(stream_id, target_serial),
        )
    }
    fn unroute_stream(&self, stream_id: u32) -> Result<(), String> {
        run("pw-metadata", &pw_metadata_unroute_args(stream_id))
    }
    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        run("wpctl", &wpctl_set_volume_args(node_id, perceptual))
    }
    fn set_node_mute(&self, node_id: u32, muted: bool) -> Result<(), String> {
        run("wpctl", &wpctl_set_mute_args(node_id, muted))
    }
    fn set_device_profile(&self, device_id: u32, profile_index: u32) -> Result<(), String> {
        run("wpctl", &wpctl_set_profile_args(device_id, profile_index))
    }
}

/// A no-op [`GraphControl`] for contexts with no daemon to drive (a `--shots`
/// render, or a test that only exercises composition).
pub struct NoopGraphControl;

impl GraphControl for NoopGraphControl {
    fn set_default_sink(&self, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn set_default_source(&self, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn route_stream(&self, _: u32, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn unroute_stream(&self, _: u32) -> Result<(), String> {
        Ok(())
    }
    fn set_node_volume(&self, _: u32, _: f32) -> Result<(), String> {
        Ok(())
    }
    fn set_node_mute(&self, _: u32, _: bool) -> Result<(), String> {
        Ok(())
    }
    fn set_device_profile(&self, _: u32, _: u32) -> Result<(), String> {
        Ok(())
    }
}

/// Source of a fresh [`AudioGraph`]. Production ([`RealEnumerator`]) shells
/// `pw-dump`; a test injects a scripted sequence, so the hotplug path can be driven
/// deterministically.
pub trait AudioEnumerator: Send + Sync {
    fn enumerate(&self) -> AudioGraph;
}

/// Production [`AudioEnumerator`]: `pw-dump` → [`parse_pw_dump`]. Empty when
/// `pw-dump` is missing or fails, so a host with no `PipeWire` shows nothing rather
/// than fabricated devices.
pub struct RealEnumerator;

impl AudioEnumerator for RealEnumerator {
    fn enumerate(&self) -> AudioGraph {
        try_enumerate().unwrap_or_default()
    }
}

pub fn enumerate() -> AudioGraph {
    RealEnumerator.enumerate()
}

/// Read the live graph once with `pw-dump`, telling a read that failed apart from a
/// graph with no devices in it.
///
/// # Errors
///
/// When `pw-dump` cannot run here, exits unsuccessfully, prints more than a graph
/// can hold, or does not finish in time.
#[cfg(not(target_arch = "wasm32"))]
pub fn try_enumerate() -> Result<AudioGraph, String> {
    dump().map(|json| parse_pw_dump(&json))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn try_read() -> Result<(AudioGraph, Vec<capture::MediaUse>), String> {
    let json = dump()?;
    Ok((parse_pw_dump(&json), capture::of_dump(&json)?))
}

/// The live graph once, as the JSON `pw-dump` prints, for readers of objects
/// [`AudioGraph`] does not model (clients and their server-attested pids).
///
/// # Errors
///
/// As [`try_enumerate`].
#[cfg(not(target_arch = "wasm32"))]
pub fn dump() -> Result<String, String> {
    let out = bounded_command::run(
        "pw-dump",
        &[],
        None,
        8 * 1024 * 1024,
        Duration::from_secs(5),
    )
    .map_err(|error| format!("pw-dump: {error}"))?;
    if !out.status.success() {
        return Err(format!("pw-dump exited with {}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(target_arch = "wasm32")]
pub fn try_enumerate() -> Result<AudioGraph, String> {
    Err("there is no audio graph on this target".into())
}

pub fn real_control() -> Arc<dyn GraphControl> {
    Arc::new(RealGraphControl)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn audio_graph_parses_real_devices_streams_and_diffs_topology() {
        {
            let volume = perceptual_from_channel_volumes(&[0.064, 0.512]).unwrap();
            assert!((volume - 0.8).abs() < 0.001);
        }
        {
            let g = parse_pw_dump(
                r#"[{
              "id": 7, "type": "PipeWire:Interface:Device",
              "info": {"props":{"device.description":"USB headset"},"params":{
                "Profile":[{"index":2}],
                "EnumProfile":[
                  {"index":1,"name":"off","description":"Off","available":"yes"},
                  {"index":2,"name":"a2dp","description":"High Fidelity Playback","available":"yes"},
                  {"index":3,"name":"hfp","description":"Headset","available":"no"}
                ]}}
            }]"#,
            );
            assert_eq!(g.hardware.len(), 1);
            assert_eq!(g.hardware[0].active_profile, Some(2));
            assert!(g.hardware[0].profiles[1].available);
            assert!(!g.hardware[0].profiles[2].available);
            assert_eq!(wpctl_set_profile_args(7, 2), ["set-profile", "7", "2"]);
        }
        {
            let g = parse_pw_dump(FIXTURE);
            assert_eq!(g.sinks.len(), 2, "two sinks: {:?}", g.sinks);
            assert_eq!(g.sources.len(), 1, "one source");
            let def = g.default_sink().expect("a default sink");
            assert_eq!(def.name, "alsa_output.pci-0000_66_00.6.analog-stereo");
            assert_eq!(def.description, "Ryzen HD Audio Controller Analoges Stereo");
            assert!(def.is_default);
            // The second sink is the routing target and is NOT the default.
            let hp = g
                .sink_by_description("Headphones")
                .expect("headphones sink");
            assert!(!hp.is_default);
            assert_eq!(
                hp.serial, 269,
                "serial is the routing handle, distinct from id"
            );
            assert_eq!(hp.id, 91);
        }
        {
            let g = parse_pw_dump(FIXTURE);
            assert_eq!(g.playback_streams.len(), 2, "two playback streams");
            let lw = g
                .playback_streams
                .iter()
                .find(|s| s.app == "LibreWolf")
                .expect("librewolf");
            assert!(lw.playback);
            assert_eq!(lw.target_id, Some(51));
            assert!(lw.media_name.as_deref().unwrap().contains("YouTube"));
            assert!(lw.label().contains("LibreWolf — "));

            let pw = g
                .playback_streams
                .iter()
                .find(|s| s.app == "pw-play")
                .expect("pw-play");
            assert_eq!(
                pw.target_id,
                Some(91),
                "the moved stream reports its non-default sink"
            );
        }
        {
            let g = parse_pw_dump(FIXTURE);
            let pw = g
                .playback_streams
                .iter()
                .find(|s| s.app == "pw-play")
                .unwrap();
            assert!(
                (pw.volume.unwrap() - 0.5).abs() < 1e-4,
                "0.125.cbrt() == 0.5, got {:?}",
                pw.volume
            );
            assert_eq!(pw.muted, Some(false));
        }
        {
            assert_eq!(parse_pw_dump(""), AudioGraph::default());
            assert_eq!(parse_pw_dump("not json"), AudioGraph::default());
            assert_eq!(parse_pw_dump("[]"), AudioGraph::default());
        }
        {
            let before = parse_pw_dump(FIXTURE);
            // Drop the Headphones sink, as an unplug would. The "after" graph is built by
            // hand from `before`, since filtering the fixture string is awkward.
            let mut after = before.clone();
            after.sinks.retain(|d| d.description != "Headphones");
            let delta = AudioGraph::diff(&before, &after);
            assert_eq!(delta.removed_sinks.len(), 1, "the headphones unplugged");
            assert_eq!(delta.removed_sinks[0].description, "Headphones");
            assert!(delta.added_sinks.is_empty());

            let replug = AudioGraph::diff(&after, &before);
            assert_eq!(replug.added_sinks.len(), 1);
            assert_eq!(replug.added_sinks[0].description, "Headphones");
            assert!(
                AudioGraph::diff(&before, &before).is_empty(),
                "no change ⇒ empty delta"
            );
        }
        {
            let g = parse_pw_dump(FIXTURE);
            let sig = g.topology_signature();
            // A volume change is not a topology change.
            let mut vol = g.clone();
            vol.playback_streams[0].volume = Some(0.1);
            assert_eq!(
                sig,
                vol.topology_signature(),
                "volume drift is not structural"
            );
            let mut added = g.clone();
            let mut ns = g.playback_streams[0].clone();
            ns.id = 999;
            ns.app = "music".into();
            added.playback_streams.push(ns);
            assert_ne!(
                sig,
                added.topology_signature(),
                "a new app changes the topology"
            );
        }
    }

    const FIXTURE: &str = include_str!("audio_pw_dump_fixture.json");

    #[test]
    fn audio_control_args_target_the_right_keys() {
        {
            assert_eq!(
                pw_metadata_route_args(84, 269),
                vec!["-n", "default", "84", "target.object", "269"],
            );
        }
        {
            assert_eq!(
                pw_metadata_unroute_args(84),
                vec!["-n", "default", "-d", "84", "target.object"],
            );
        }
        {
            assert_eq!(
                wpctl_set_volume_args(84, 0.5),
                vec!["set-volume", "84", "0.50"]
            );
            assert_eq!(wpctl_set_mute_args(84, true), vec!["set-mute", "84", "1"]);
            assert_eq!(wpctl_set_mute_args(84, false), vec!["set-mute", "84", "0"]);
            assert_eq!(wpctl_set_default_args(91), vec!["set-default", "91"]);
        }
    }

    #[test]
    fn echo_cancelled_microphone_nodes_are_neither_devices_nor_apps() {
        let g = parse_pw_dump(
            r#"[
            {"id":10,"type":"PipeWire:Interface:Node","info":{"props":{
              "media.class":"Audio/Source","node.name":"alsa_input.usb-mic","object.serial":110}}},
            {"id":11,"type":"PipeWire:Interface:Node","info":{"props":{
              "media.class":"Audio/Source","node.name":"tilo_echo_cancel_source","object.serial":111,
              "node.description":"Echo-cancelled microphone"}}},
            {"id":12,"type":"PipeWire:Interface:Node","info":{"props":{
              "media.class":"Stream/Input/Audio","node.name":"tilo_echo_cancel_capture","object.serial":112}}},
            {"id":13,"type":"PipeWire:Interface:Node","info":{"props":{
              "media.class":"Stream/Input/Audio","node.name":"tilo_echo_cancel_reference","object.serial":113}}}
        ]"#,
        );
        let sources: Vec<_> = g.sources.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(sources, ["alsa_input.usb-mic"]);
        assert!(g.record_streams.is_empty(), "{:?}", g.record_streams);
    }

    #[test]
    fn exact_audio_source_requires_a_real_bounded_serial() {
        for class in ["Audio/Source", "Audio/Sink"] {
            let missing = serde_json::json!([{"id": 42, "type": "PipeWire:Interface:Node",
                "info": {"props": {"media.class": class}}}]);
            let graph = parse_pw_dump(&missing.to_string());
            assert!(graph.sources.is_empty() && graph.sinks.is_empty());
            for serial in [
                serde_json::Value::Null,
                serde_json::json!(4_294_967_296u64),
                serde_json::json!("4294967296"),
                serde_json::json!(-1),
                serde_json::json!(1.5),
            ] {
                let input = serde_json::json!([{"id": 42, "type": "PipeWire:Interface:Node",
                    "info": {"props": {"media.class": class, "object.serial": serial}}}]);
                let graph = parse_pw_dump(&input.to_string());
                assert!(
                    graph.sources.is_empty() && graph.sinks.is_empty(),
                    "a reusable node ID cannot replace missing/invalid object.serial: {input}"
                );
            }
            for serial in [
                serde_json::json!(246),
                serde_json::json!("246"),
                serde_json::json!(u32::MAX),
            ] {
                let input = serde_json::json!([{"id": 42, "type": "PipeWire:Interface:Node",
                    "info": {"props": {"media.class": class, "object.serial": serial}}}]);
                let graph = parse_pw_dump(&input.to_string());
                let device = graph
                    .sources
                    .first()
                    .or_else(|| graph.sinks.first())
                    .unwrap();
                assert_eq!(device.id, 42);
                assert_eq!(device.serial, as_u32(&serial).unwrap());
            }
        }
    }
}
