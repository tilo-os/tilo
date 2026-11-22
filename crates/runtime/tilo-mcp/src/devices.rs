//! The person's devices as an agent's tool lists them, and the names they go by. The
//! connections between them are `tilo-device-mcp`'s.

use tilo_user_identity::DeviceCert;

/// The longest device name the person is shown, and a `--device` is matched
/// against.
const MAX_NAME: usize = 64;

/// A device as the person reads it: its label, or the start of its key when
/// it has none.
#[must_use]
pub fn device_name(device: &DeviceCert) -> String {
    let label: String = device
        .device_label
        .trim()
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_NAME)
        .collect();
    if label.is_empty() {
        short_id(&device.device_public_key)
    } else {
        label
    }
}

/// The first four bytes of a device key, as hex: enough to tell two devices
/// of one person with the same label apart.
fn short_id(key: &[u8; 32]) -> String {
    hex::encode(key.get(..4).unwrap_or_default())
}

/// The agent id a client admitted from another device works as: its own id
/// (`claude-code`), then `@` and the device it came from
/// (`claude-code@studio-laptop-3f2a9c1b`), so what it does is told apart from
/// what the same client does on this device, and from another device's.
#[must_use]
pub fn agent_id(client: &str, device: &DeviceCert) -> String {
    let mut slug = String::new();
    for character in device.device_label.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            slug.push(character);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
        if slug.len() >= 40 {
            break;
        }
    }
    let slug = slug.trim_matches('-');
    let client: String = client.chars().take(64).collect();
    let short = short_id(&device.device_public_key);
    if slug.is_empty() {
        format!("{client}@{short}")
    } else {
        format!("{client}@{slug}-{short}")
    }
}

/// One of the person's devices, and whether its server answers this one now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
    pub device: DeviceCert,
    /// It is the device asking.
    pub this: bool,
    /// Whether its server would open for this device now, or why not.
    pub answer: Result<(), String>,
}

/// The person's devices and whether each answers now, as an agent's tool
/// lists them.
pub trait DeviceDirectory: Send + Sync {
    /// Every device of the person this one knows, with whether it answers.
    fn reach(&self) -> std::pin::Pin<Box<dyn Future<Output = Vec<Reach>> + Send + '_>>;
}

/// The tool an agent lists the person's devices with.
pub const DEVICES_TOOL: &str = "tilo_devices";

/// [`DEVICES_TOOL`]'s definition.
#[must_use]
pub fn tool_definition() -> serde_json::Value {
    serde_json::json!({
        "name": DEVICES_TOOL,
        "description": "List your other devices and whether each answers now. An agent works on another device by connecting to it: `tilo agent connect claude --device NAME`, or `tilo-runtime --connect --device NAME`.",
        "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "annotations": {"readOnlyHint": true, "openWorldHint": true}
    })
}

/// The devices as the tool answers them: one line each, with its name, its
/// endpoint id and whether it answers.
#[must_use]
pub fn listing(reach: &[Reach]) -> String {
    let mut lines = Vec::with_capacity(reach.len().saturating_add(1));
    if reach.iter().all(|reach| reach.this) {
        lines.push("No other device of yours is known here.".to_owned());
    }
    for reach in reach {
        let state = match (&reach.answer, reach.this) {
            (_, true) => "this device".to_owned(),
            (Ok(()), false) => "reachable".to_owned(),
            (Err(why), false) => format!("not reachable: {why}"),
        };
        lines.push(format!(
            "{}  {}  {state}",
            device_name(&reach.device),
            hex::encode(reach.device.device_public_key)
        ));
    }
    lines.join("\n")
}
