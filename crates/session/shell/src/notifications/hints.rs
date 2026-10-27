//! What one `Notify` call's arguments and hints say: the urgency, the sound a
//! notification asks for, whether it is a call ringing now, its badge, its
//! artwork, its contact and sender name, its sharing limit, and the
//! `[key, label, …]` actions array — each parsed from the freedesktop spec's
//! wire form and bounded so a hostile sender cannot grow the chrome.

#[cfg(any(test, target_os = "linux"))]
use std::collections::HashMap;

#[cfg(any(test, target_os = "linux"))]
use zbus::zvariant::OwnedValue;

#[cfg(any(test, target_os = "linux"))]
use super::NotificationSharing;

/// A sender-supplied `image-data` hint, decoded and bounded: straight-alpha
/// RGBA, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Parse the `urgency` hint (a `y`/byte); defaults to `1` (normal) when absent or the wrong type — never refuse a notification over a bad hint.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_urgency(hints: &HashMap<String, OwnedValue>) -> u8 {
    hints
        .get("urgency")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(1)
}

/// The system sound a notification asks for: `None` when the sender set
/// `suppress-sound`, otherwise the event its `category` hint names.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_notification_sound(
    hints: &HashMap<String, OwnedValue>,
) -> Option<settings_store::system_sounds::SystemSoundEvent> {
    let suppressed = hints
        .get("suppress-sound")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false);
    let category = hints
        .get("category")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| String::try_from(value).ok());
    (!suppressed).then(|| {
        settings_store::system_sounds::SystemSoundEvent::for_notification_category(
            category.as_deref(),
        )
    })
}

/// Whether the sender filed the notification as a call ringing now.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_incoming_call(hints: &HashMap<String, OwnedValue>) -> bool {
    hints
        .get("category")
        .and_then(|value| <&str>::try_from(value).ok())
        == Some("call.incoming")
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_badge_count(hints: &HashMap<String, OwnedValue>) -> Option<u32> {
    hints
        .get("x-tilo-badge-count")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(9_999))
}

/// A bounded text hint used only to find artwork, never as identity.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_icon_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    let value = hints
        .get(key)?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 4096 && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

/// Decode the freedesktop `image-data` hint, rejecting anything that is not
/// 8-bit RGB/RGBA at a size chrome can paint.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_image_data(hints: &HashMap<String, OwnedValue>) -> Option<IncomingImageData> {
    let structure =
        zbus::zvariant::Structure::try_from(hints.get("image-data")?.try_clone().ok()?).ok()?;
    let mut fields = structure.into_fields();
    if fields.len() != 7 {
        return None;
    }
    let width = i32::try_from(fields.remove(0)).ok()?;
    let height = i32::try_from(fields.remove(0)).ok()?;
    let rowstride = i32::try_from(fields.remove(0)).ok()?;
    let _has_alpha = bool::try_from(fields.remove(0)).ok()?;
    let bits = i32::try_from(fields.remove(0)).ok()?;
    let channels = i32::try_from(fields.remove(0)).ok()?;
    let bytes = fields.remove(0).downcast::<Vec<u8>>().ok()?;
    let (width, height, rowstride) = (
        u32::try_from(width).ok()?,
        u32::try_from(height).ok()?,
        usize::try_from(rowstride).ok()?,
    );
    let channels = usize::try_from(channels).ok()?;
    if bits != 8
        || !(3..=4).contains(&channels)
        || width == 0
        || height == 0
        || width > 512 // allow-breakpoint: an icon's pixel size, not a layout
        || height > 512 // allow-breakpoint: an icon's pixel size, not a layout
        || rowstride < (width as usize).checked_mul(channels)?
    {
        return None;
    }
    let needed = rowstride.checked_mul(height as usize)?;
    if bytes.len() < needed {
        return None;
    }
    let mut rgba = Vec::with_capacity(
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?,
    );
    for row in 0..height as usize {
        let line = bytes.get(row * rowstride..row * rowstride + rowstride)?;
        for pixel in 0..width as usize {
            let at = pixel * channels;
            let r = *line.get(at)?;
            let g = *line.get(at + 1)?;
            let b = *line.get(at + 2)?;
            let a = if channels == 4 {
                *line.get(at + 3)?
            } else {
                255
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(IncomingImageData {
        width,
        height,
        rgba,
    })
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_contact_endpoint(hints: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = hints
        .get("x-tilo-contact-handle")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    (value.len() <= 320 && !value.chars().any(char::is_control)).then_some(value)
}

/// The `x-tilo-sender-name` hint: a person's display name, one line and
/// bounded like a contact handle.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_sender_name(hints: &HashMap<String, OwnedValue>) -> Option<String> {
    let value = hints
        .get("x-tilo-sender-name")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 320 && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn parse_notification_sharing(
    hints: &HashMap<String, OwnedValue>,
) -> NotificationSharing {
    let Some(value) = hints.get("x-tilo-device-sharing") else {
        return NotificationSharing::Full;
    };
    match value
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())
        .as_deref()
    {
        Some("full") => NotificationSharing::Full,
        Some("redacted") => NotificationSharing::Redacted,
        _ => NotificationSharing::LocalOnly,
    }
}

/// Un-flatten the spec's `[key1, label1, key2, label2, …]` actions array into `(key, label)` pairs, dropping a dangling trailing key.
#[cfg(any(test, target_os = "linux"))]
pub(super) fn pair_actions(actions: &[String]) -> Vec<(String, String)> {
    actions
        .chunks_exact(2)
        .filter_map(|c| {
            let [key, label] = c else {
                return None; // chunks_exact(2) guarantees len == 2
            };
            Some((key.clone(), label.clone()))
        })
        .collect()
}
