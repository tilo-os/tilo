//! The audio graph client: enumerate the PipeWire/WirePlumber node graph, move
//! a single application's stream between output devices, adjust per-stream
//! volume/mute, switch the default device, and diff two enumerations to react
//! to hotplug.
//!
//! ## Implementation
//!
//! Enumeration reads `pw-dump` (the graph as JSON), routing writes the
//! `WirePlumber` `target.object` metadata via `pw-metadata`, and per-node
//! volume/mute + the default device go through `wpctl`. Every one of those is
//! the native `PipeWire` path, reached through the daemon's command-line tools
//! rather than by linking `libpipewire-0.3`. This keeps the crate buildable
//! without `PipeWire` development headers; the tools are needed at runtime. On a
//! machine with no `PipeWire`, the enumerator returns an empty graph.
//!
//! ## The routing mechanism
//!
//! `WirePlumber` exposes "moving streams" as metadata on the `default` metadata
//! store: setting the key `target.object` on a stream node's id to a target
//! device's `object.serial` relinks the stream to that device; deleting the key
//! returns the stream to following the default device. This is what
//! `linking.allow-moving-streams` gates, and what the settings pane drives so
//! the music app can play to the headphones while the browser stays on the
//! speakers.
//!
//! Parsing, command construction, and hotplug diff are pure functions. The
//! shelling-out [`RealGraphControl`] and [`RealEnumerator`] implementations
//! invoke the command-line tools.

#[cfg(not(target_arch = "wasm32"))]
use std::process::Command;
use std::sync::Arc;
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use serde_json::Value;

/// A sink (output) or source (input) device in the audio graph.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    /// `PipeWire` node id — the handle `wpctl set-default <id>` takes.
    pub id: u32,
    /// `object.serial` — the stable handle stream routing (`target.object`)
    /// takes. Distinct from `id`: a device keeps its `node.name` across a
    /// hotplug cycle but is assigned a fresh serial each time it appears.
    pub serial: u32,
    /// `node.name` — the machine identifier the default-device metadata stores
    /// (e.g. `alsa_output.pci-0000_66_00.6.analog-stereo`).
    pub name: String,
    /// Human-readable label (`node.description`, falling back to `node.name`).
    pub description: String,
    /// Whether this device is the current system default for its class.
    pub is_default: bool,
    /// Perceptual volume in `0.0..=1.0` (the cube root of the raw linear
    /// channel volume, matching what `wpctl` displays), if known.
    pub volume: Option<f32>,
    pub muted: Option<bool>,
    /// Number of channels reported by Props.channelVolumes. Balance and
    /// channel-test controls must not appear unless this is at least two.
    pub channels: usize,
    /// Active Bluetooth codec when the `PipeWire` `BlueZ` node advertises it.
    /// Informational: codec selection is exposed only through available profiles.
    pub codec: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceProfile {
    pub index: u32,
    pub name: String,
    pub description: String,
    pub available: bool,
}

/// A `PipeWire` Device (sound card / Bluetooth transport) and the profiles the
/// daemon says can actually be selected with `wpctl set-profile`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioHardwareDevice {
    pub id: u32,
    pub description: String,
    pub active_profile: Option<u32>,
    pub profiles: Vec<DeviceProfile>,
}

/// A per-application stream node (one app's playback or capture).
#[derive(Clone, Debug, PartialEq)]
pub struct AudioStream {
    /// `PipeWire` node id — the handle routing / `wpctl set-volume|set-mute`
    /// take.
    pub id: u32,
    pub serial: u32,
    /// Best-effort application label (`application.name`, falling back to
    /// `node.name`).
    pub app: String,
    /// What the stream is playing, when the app advertised it (`media.name`).
    pub media_name: Option<String>,
    /// `true` for a playback stream (`Stream/Output/Audio`, feeds a sink),
    /// `false` for a capture stream (`Stream/Input/Audio`, reads a source).
    pub playback: bool,
    /// The device node id this stream is currently linked to, if any — a
    /// playback stream's sink, or a capture stream's source. Derived from the
    /// graph's `Link` objects.
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

/// The enumerated audio graph: devices and per-application streams.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioGraph {
    pub sinks: Vec<AudioDevice>,
    pub sources: Vec<AudioDevice>,
    pub playback_streams: Vec<AudioStream>,
    pub record_streams: Vec<AudioStream>,
    pub hardware: Vec<AudioHardwareDevice>,
}

impl AudioGraph {
    /// The current default sink, if the graph advertised one.
    pub fn default_sink(&self) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.is_default)
    }
    /// The current default source, if any.
    pub fn default_source(&self) -> Option<&AudioDevice> {
        self.sources.iter().find(|d| d.is_default)
    }
    /// A sink by its node id.
    pub fn sink_by_id(&self, id: u32) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.id == id)
    }
    /// A sink by human description (the label the picker shows).
    pub fn sink_by_description(&self, desc: &str) -> Option<&AudioDevice> {
        self.sinks.iter().find(|d| d.description == desc)
    }

    /// A stable, cheap fingerprint of the graph's *topology* — the set of
    /// devices (by name) and streams (by id+app) present — deliberately
    /// excluding volume/mute so a volume drift does not read as a structural
    /// change. Two enumerations with the same signature carry the same rows,
    /// so the UI need not rebuild; a different signature means a device or app
    /// appeared or disappeared (hotplug / a stream started or stopped).
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

    /// Diff two enumerations into the devices that appeared and disappeared —
    /// the hotplug delta a caller reacts to (offer the new headset, drop the
    /// unplugged one). Identity is `node.name`, which is stable for the same
    /// physical device across an unplug/replug.
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

/// The device add/remove delta between two [`AudioGraph`] enumerations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphDelta {
    pub added_sinks: Vec<AudioDevice>,
    pub removed_sinks: Vec<AudioDevice>,
    pub added_sources: Vec<AudioDevice>,
    pub removed_sources: Vec<AudioDevice>,
}

impl GraphDelta {
    /// Whether any device appeared or disappeared.
    pub fn is_empty(&self) -> bool {
        self.added_sinks.is_empty()
            && self.removed_sinks.is_empty()
            && self.added_sources.is_empty()
            && self.removed_sources.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Parsing `pw-dump`
// ---------------------------------------------------------------------------

/// Coerce a JSON value that `PipeWire` renders as a bare number (sometimes a
/// string) into a `u32` node id / serial.
fn as_u32(v: &Value) -> Option<u32> {
    v.as_u64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        .and_then(|n| u32::try_from(n).ok())
}

/// The perceptual volume `wpctl` shows is the cube root of `PipeWire`'s raw
/// linear channel volume; invert that so an enumerated `channelVolumes` matches
/// the slider position a user set. `wpctl set-volume` takes the perceptual
/// value back, so the round trip is exact.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a cube-root perceptual volume is a small, near-1.0 ratio; f32's \
              range is never at risk"
)]
fn perceptual_from_channel_volumes(cv: &[f64]) -> Option<f32> {
    // Treat the loudest channel as the device's base volume. Reading the
    // first channel made a right-balanced sink appear quieter on every graph
    // refresh; a persistent balance policy would then attenuate it again and
    // again. Pulse/WirePlumber likewise expose the loud side as the overall
    // volume while balance describes the channel ratio.
    cv.iter()
        .copied()
        .reduce(f64::max)
        .map(|v| (v.max(0.0)).cbrt() as f32)
}

/// Read a node's first `Props` param that carries channel volumes, yielding
/// `(perceptual_volume, muted)`.
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

/// Parse a `pw-dump` JSON document into an [`AudioGraph`]. Robust against the
/// surrounding graph: it reads the default-device metadata, the `Link` objects
/// (to resolve where each stream is actually routed), and the audio `Node`s,
/// ignoring video, MIDI, clients, ports, factories and the rest. An empty or
/// malformed document yields an empty graph — never fabricated entries.
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

/// Pass 1 — the default sink/source node names from the "default" metadata.
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

/// Pass 2 — the links, as (output.node, input.node) pairs. A playback
/// stream's output links into its sink; a capture stream's input links from
/// its source.
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

/// Pass 3 — the audio nodes: sinks, sources, and streams (plus hardware
/// Devices, for their selectable profiles).
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

/// Parse one `PipeWire:Interface:Device` object into its selectable-profile
/// summary. `None` when it carries no usable id.
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

/// `node.name` prefix of every node the session's echo-cancellation client
/// creates (os/usr/share/pipewire/tilo-echo-cancel.conf).
pub const ECHO_CANCEL_NODE_PREFIX: &str = "tilo_echo_cancel_";

/// The echo-cancelled microphone calls are routed to.
pub const ECHO_CANCEL_SOURCE_NAME: &str = "tilo_echo_cancel_source";

/// Parse one audio `Node` object and push it into `graph` as a device
/// (sink/source) or a stream (playback/record), whichever `media.class` says
/// it is. Ignored (no push) when it carries no usable id or an unrecognised
/// `media.class`.
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
    // Serial is a PipeWire object identity; a reusable node id is not a substitute.
    // The public graph currently supports u32 serials. Larger values are omitted.
    let Some(serial) = getp("object.serial").and_then(as_u32) else {
        return;
    };
    let name = getp("node.name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // The echo-cancelled microphone and its internal streams are a session
    // transform over the real devices, not a device or an app: listing them
    // would offer a microphone that records itself as the default.
    if name.starts_with(ECHO_CANCEL_NODE_PREFIX) {
        return;
    }
    let (volume, muted) = node_volume_mute(o);

    match media_class {
        "Audio/Sink" | "Audio/Source" => {
            let description = getp("node.description")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
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
            // A playback stream links out→sink; a capture stream links
            // source→in. Resolve the device end accordingly.
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

// ---------------------------------------------------------------------------
// Command construction (pure) + the shelling-out production impls
// ---------------------------------------------------------------------------

/// `wpctl set-default <id>` — switch the default sink or source. The default
/// metadata records the device's `node.name`, so a stream following the default
/// follows the switch.
pub fn wpctl_set_default_args(node_id: u32) -> Vec<String> {
    vec!["set-default".into(), node_id.to_string()]
}

/// `pw-metadata -n default <stream-id> target.object <serial>` — move a stream
/// node to a specific device. `WirePlumber` observes the metadata and relinks the
/// stream, so this is the real graph edit, not a display hint.
pub fn pw_metadata_route_args(stream_id: u32, target_serial: u32) -> Vec<String> {
    vec![
        "-n".into(),
        "default".into(),
        stream_id.to_string(),
        "target.object".into(),
        target_serial.to_string(),
    ]
}

/// `pw-metadata -n default -d <stream-id> target.object` — clear a stream's
/// explicit target so it follows the default device again.
pub fn pw_metadata_unroute_args(stream_id: u32) -> Vec<String> {
    vec![
        "-n".into(),
        "default".into(),
        "-d".into(),
        stream_id.to_string(),
        "target.object".into(),
    ]
}

/// `wpctl set-volume <id> <fraction>` — set any node's (device *or* stream)
/// perceptual volume. Taking a node id is what makes per-application volume
/// possible: the same call that sets the sink's volume sets one app's.
pub fn wpctl_set_volume_args(node_id: u32, perceptual: f32) -> Vec<String> {
    let frac = perceptual.clamp(0.0, 1.5); // wpctl allows >1.0 (boost); cap sanely
    vec![
        "set-volume".into(),
        node_id.to_string(),
        format!("{frac:.2}"),
    ]
}

/// `wpctl set-mute <id> 1|0` — mute/unmute any node.
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

/// Side-effect interface for editing the audio graph. The production impl
/// ([`RealGraphControl`]) shells out to `wpctl` / `pw-metadata`; tests inject a
/// recorder so the settings pane's dispatch can be asserted without a daemon.
pub trait GraphControl: Send + Sync {
    /// Switch the default sink (output).
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `wpctl` is unavailable or
    /// rejects `node_id`.
    fn set_default_sink(&self, node_id: u32) -> Result<(), String>;
    /// Switch the default source (input).
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `wpctl` is unavailable or
    /// rejects `node_id`.
    fn set_default_source(&self, node_id: u32) -> Result<(), String>;
    /// Route a stream node to a device by the device's `object.serial`.
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `pw-metadata` is unavailable
    /// or rejects the stream id or target serial.
    fn route_stream(&self, stream_id: u32, target_serial: u32) -> Result<(), String>;
    /// Clear a stream's explicit target (follow the default device).
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `pw-metadata` is unavailable
    /// or rejects the stream id.
    fn unroute_stream(&self, stream_id: u32) -> Result<(), String>;
    /// Set a node's perceptual volume (`0.0..=1.0`).
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `wpctl` is unavailable or
    /// rejects `node_id`.
    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String>;
    /// Mute or unmute a node.
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `wpctl` is unavailable or
    /// rejects `node_id`.
    fn set_node_mute(&self, node_id: u32, muted: bool) -> Result<(), String>;
    /// Switch a hardware device to one of its enumerated profiles.
    ///
    /// # Errors
    ///
    /// Returns the tool's diagnostic message if `wpctl` is unavailable or
    /// rejects `device_id`/`profile_index`.
    fn set_device_profile(&self, device_id: u32, profile_index: u32) -> Result<(), String>;
}

/// Run `bin` with `args`, mapping a missing binary or non-zero exit to the
/// daemon's error string — the same best-effort shape `os_actions` uses.
#[cfg(not(target_arch = "wasm32"))]
fn run(bin: &str, args: &[String]) -> Result<(), String> {
    let mut command = Command::new(bin);
    command.args(args);
    let out = bounded_command::run_prepared(command, None, 64 * 1024, Duration::from_secs(3))
        .map_err(|e| format!("{bin} control failed: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(sanitize_diagnostic(&out.stderr))
    }
}

#[cfg(target_arch = "wasm32")]
fn run(_: &str, _: &[String]) -> Result<(), String> {
    Err("PipeWire audio graph control is unsupported on this platform".to_owned())
}

#[cfg(not(target_arch = "wasm32"))]
fn sanitize_diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .filter(|character| !character.is_control() || matches!(character, ' ' | '\n' | '\t'))
        .take(512)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Production [`GraphControl`]: `wpctl` for defaults + per-node volume/mute,
/// `pw-metadata` for stream routing.
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
/// render, or a test that only exercises composition). Records nothing.
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

/// Source of a fresh [`AudioGraph`] snapshot. The production impl
/// ([`RealEnumerator`]) shells `pw-dump`; a test injects a scripted sequence so
/// the hotplug path can be driven deterministically.
pub trait AudioEnumerator: Send + Sync {
    fn enumerate(&self) -> AudioGraph;
}

/// Production [`AudioEnumerator`]: `pw-dump` → [`parse_pw_dump`]. Returns an
/// empty graph when `pw-dump` is missing or fails, so a host with no `PipeWire`
/// shows nothing rather than fabricated devices.
pub struct RealEnumerator;

impl AudioEnumerator for RealEnumerator {
    #[cfg(not(target_arch = "wasm32"))]
    fn enumerate(&self) -> AudioGraph {
        let Ok(out) = bounded_command::run(
            "pw-dump",
            &[],
            None,
            8 * 1024 * 1024,
            Duration::from_secs(5),
        ) else {
            return AudioGraph::default();
        };
        if !out.status.success() {
            return AudioGraph::default();
        }
        parse_pw_dump(&String::from_utf8_lossy(&out.stdout))
    }

    #[cfg(target_arch = "wasm32")]
    fn enumerate(&self) -> AudioGraph {
        AudioGraph::default()
    }
}

/// Convenience: enumerate the live graph once via [`RealEnumerator`].
pub fn enumerate() -> AudioGraph {
    RealEnumerator.enumerate()
}

/// Shorthand for the boxed control the settings pane holds.
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
        // Phase `device_volume_uses_loud_channel_so_balance_does_not_decay`.
        {
            let volume = perceptual_from_channel_volumes(&[0.064, 0.512]).unwrap();
            assert!((volume - 0.8).abs() < 0.001);
        }
        // Phase `parses_only_real_available_device_profiles`.
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
        // Phase `parses_sinks_sources_and_default`.
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
        // Phase `parses_streams_with_their_current_route`.
        {
            let g = parse_pw_dump(FIXTURE);
            assert_eq!(g.playback_streams.len(), 2, "two playback streams");
            let lw = g
                .playback_streams
                .iter()
                .find(|s| s.app == "LibreWolf")
                .expect("librewolf");
            assert!(lw.playback);
            // LibreWolf is linked to the default sink (node 51).
            assert_eq!(lw.target_id, Some(51));
            assert!(lw.media_name.as_deref().unwrap().contains("YouTube"));
            assert!(lw.label().contains("LibreWolf — "));

            // pw-play was explicitly moved onto the Headphones sink (node 91) — the
            // per-app routing this feature exists to do, read back from the graph.
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
        // Phase `parses_perceptual_volume_and_mute`.
        {
            let g = parse_pw_dump(FIXTURE);
            // The pw-play stream's raw channelVolumes were 0.125; cube root is
            // exactly 0.5 — the perceptual value wpctl shows and set-volume takes.
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
        // Phase `empty_document_is_empty_graph_not_fabricated`.
        {
            assert_eq!(parse_pw_dump(""), AudioGraph::default());
            assert_eq!(parse_pw_dump("not json"), AudioGraph::default());
            assert_eq!(parse_pw_dump("[]"), AudioGraph::default());
        }
        // Phase `diff_reports_hotplugged_devices`.
        {
            let before = parse_pw_dump(FIXTURE);
            // Drop the Headphones sink → simulate an unplug; the graph now has one
            // sink. Reparse a fixture without node 91 by filtering the string is
            // awkward, so build the "after" graph by hand from `before`.
            let mut after = before.clone();
            after.sinks.retain(|d| d.description != "Headphones");
            let delta = AudioGraph::diff(&before, &after);
            assert_eq!(delta.removed_sinks.len(), 1, "the headphones unplugged");
            assert_eq!(delta.removed_sinks[0].description, "Headphones");
            assert!(delta.added_sinks.is_empty());

            // And the reverse — replug — reports it as added.
            let replug = AudioGraph::diff(&after, &before);
            assert_eq!(replug.added_sinks.len(), 1);
            assert_eq!(replug.added_sinks[0].description, "Headphones");
            assert!(
                AudioGraph::diff(&before, &before).is_empty(),
                "no change ⇒ empty delta"
            );
        }
        // Phase `topology_signature_ignores_volume_but_tracks_structure`.
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
            // A new stream is.
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

    /// A faithful, trimmed capture of a real `pw-dump` from a running
    /// `PipeWire` 1.6 / `WirePlumber` host: two sinks (the built-in analog stereo,
    /// default; and a second "Headphones" device), the default source, and two
    /// playback streams — `LibreWolf` following the default sink, and a `pw-play`
    /// stream that was moved onto the Headphones sink via `target.object`. The
    /// field shapes (`media.class`, `object.serial`, `Link` props,
    /// `info.params.Props[].channelVolumes`/`mute`, the default metadata) are
    /// exactly as `pw-dump` emits them.
    const FIXTURE: &str = include_str!("audio_pw_dump_fixture.json");

    #[test]
    fn audio_control_args_target_the_right_keys() {
        // Phase `route_args_target_the_serial_on_the_default_store`.
        {
            // Moving stream 84 to a sink whose object.serial is 269.
            assert_eq!(
                pw_metadata_route_args(84, 269),
                vec!["-n", "default", "84", "target.object", "269"],
            );
        }
        // Phase `unroute_args_delete_the_target_key`.
        {
            assert_eq!(
                pw_metadata_unroute_args(84),
                vec!["-n", "default", "-d", "84", "target.object"],
            );
        }
        // Phase `volume_and_mute_and_default_args`.
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
