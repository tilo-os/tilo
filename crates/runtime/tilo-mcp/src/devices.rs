//! The id an agent working for a key of the person's device goes by.

use tilo_user_identity::DeviceCert;

/// The first four bytes of a device key, as hex: enough to tell two devices
/// of one person with the same label apart.
fn short_id(key: &[u8; 32]) -> String {
    hex::encode(key.get(..4).unwrap_or_default())
}

/// The agent id a client admitted for a device's key works as: its own id
/// (`claude-code`), then `@` and the device (`claude-code@studio-laptop-3f2a9c1b`),
/// so what it does is told apart from what the same client does for another key.
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
