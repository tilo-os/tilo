//! What one `Notify` call's arguments and hints say: the urgency, the
//! category and whether the sender asked for silence, whether it is a call
//! ringing now, its badge, its artwork, its contact and sender name, and the
//! `[key, label, …]` actions array — each parsed from the freedesktop spec's
//! wire form and bounded so a hostile sender cannot grow what the session
//! keeps.

use std::collections::HashMap;

use zbus::zvariant::OwnedValue;

/// The spec arguments' hints, as `Notify` receives them.
pub(crate) type Hints = HashMap<String, OwnedValue>;

/// The category of a call ringing now.
const INCOMING_CALL: &str = "call.incoming";
/// The category of an Android app's ongoing notification, which lasts
/// while its sender stays on the bus.
pub(crate) const ONGOING: &str = "x-tilo.android-ongoing";

/// A sender-supplied `image-data` hint, decoded and bounded: straight-alpha
/// RGBA, row-major, `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Parse the `urgency` hint (a `y`/byte); defaults to `1` (normal) when
/// absent or the wrong type — never refuse a notification over a bad hint.
pub(crate) fn parse_urgency(hints: &Hints) -> u8 {
    hints
        .get("urgency")
        .and_then(|v| v.try_clone().ok())
        .and_then(|v| u8::try_from(v).ok())
        .unwrap_or(1)
}

/// The `category` hint, one bounded line.
pub(crate) fn parse_category(hints: &Hints) -> Option<String> {
    let value = hints
        .get("category")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    (!value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control))
        .then_some(value)
}

/// Whether the sender set `suppress-sound`.
pub(crate) fn parse_suppress_sound(hints: &Hints) -> bool {
    hints
        .get("suppress-sound")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

/// Whether the sender filed the notification as a call ringing now.
pub(crate) fn parse_incoming_call(hints: &Hints) -> bool {
    hints
        .get("category")
        .and_then(|value| <&str>::try_from(value).ok())
        == Some(INCOMING_CALL)
}

/// Whether the sender filed the notification as an ongoing one.
pub(crate) fn parse_ongoing(hints: &Hints) -> bool {
    hints
        .get("category")
        .and_then(|value| <&str>::try_from(value).ok())
        == Some(ONGOING)
}

pub(crate) fn parse_badge_count(hints: &Hints) -> Option<u32> {
    hints
        .get("x-tilo-badge-count")
        .and_then(|value| value.try_clone().ok())
        .and_then(|value| u32::try_from(value).ok())
        .map(|value| value.min(9_999))
}

/// A bounded text hint used only to find artwork, never as identity.
pub(crate) fn parse_icon_hint(hints: &Hints, key: &str) -> Option<String> {
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
/// 8-bit RGB/RGBA at a size the session can paint.
pub(crate) fn parse_image_data(hints: &Hints) -> Option<ImageData> {
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
    for line in bytes.chunks_exact(rowstride).take(height as usize) {
        for pixel in line.chunks_exact(channels).take(width as usize) {
            let [r, g, b, rest @ ..] = pixel else {
                return None;
            };
            let a = rest.first().copied().unwrap_or(255);
            rgba.extend_from_slice(&[*r, *g, *b, a]);
        }
    }
    Some(ImageData {
        width,
        height,
        rgba,
    })
}

pub(crate) fn parse_contact_endpoint(hints: &Hints) -> Option<String> {
    let value = hints
        .get("x-tilo-contact-handle")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    (value.len() <= 320 && !value.chars().any(char::is_control)).then_some(value)
}

/// The `x-tilo-sender-name` hint: a person's display name, one line and
/// bounded like a contact handle.
pub(crate) fn parse_sender_name(hints: &Hints) -> Option<String> {
    let value = hints
        .get("x-tilo-sender-name")?
        .try_clone()
        .ok()
        .and_then(|value| String::try_from(value).ok())?;
    let value = value.trim();
    (!value.is_empty() && value.len() <= 320 && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

/// Un-flatten the spec's `[key1, label1, key2, label2, …]` actions array
/// into `(key, label)` pairs, dropping a dangling trailing key.
pub(crate) fn pair_actions(actions: &[String]) -> Vec<(String, String)> {
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a hint that fails to build fails the test"
)]
mod tests {
    use zbus::zvariant::{Str, Value};

    use super::*;

    #[test]
    fn image_data_hints_are_decoded_and_bounded() {
        let mut hints = HashMap::new();
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                2_i32,
                1_i32,
                8_i32,
                true,
                8_i32,
                4_i32,
                vec![255_u8, 0, 0, 255, 0, 255, 0, 255],
            )))
            .expect("image-data"),
        );
        let image = parse_image_data(&hints).expect("RGBA is decoded");
        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(image.rgba, [255, 0, 0, 255, 0, 255, 0, 255]);

        // Three channels gain an opaque alpha.
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                1_i32,
                1_i32,
                4_i32,
                false,
                8_i32,
                3_i32,
                vec![1_u8, 2, 3, 0],
            )))
            .expect("image-data"),
        );
        assert_eq!(
            parse_image_data(&hints).map(|image| image.rgba),
            Some(vec![1, 2, 3, 255])
        );

        // More than 8 bits per sample is not something the card can paint.
        hints.insert(
            "image-data".to_string(),
            OwnedValue::try_from(Value::new((
                1_i32,
                1_i32,
                4_i32,
                false,
                16_i32,
                3_i32,
                vec![0_u8; 32],
            )))
            .expect("image-data"),
        );
        assert!(parse_image_data(&hints).is_none());
    }

    #[test]
    fn actions_pair_up_and_drop_a_dangling_key() {
        let actions = vec![
            "default".to_string(),
            "Open".to_string(),
            "snooze".to_string(),
            "Snooze".to_string(),
            "dangling".to_string(),
        ];
        let pairs = pair_actions(&actions);
        assert_eq!(
            pairs,
            vec![
                ("default".to_string(), "Open".to_string()),
                ("snooze".to_string(), "Snooze".to_string()),
            ]
        );
    }

    #[test]
    fn urgency_hint_defaults_to_normal() {
        let hints: Hints = HashMap::new();
        assert_eq!(parse_urgency(&hints), 1);
        let mut hints: Hints = HashMap::new();
        hints.insert("urgency".to_string(), OwnedValue::from(2u8));
        assert_eq!(parse_urgency(&hints), 2);
    }

    #[test]
    fn badge_and_contact_hints_are_bounded() {
        let mut hints = HashMap::new();
        hints.insert("x-tilo-badge-count".into(), OwnedValue::from(u32::MAX));
        hints.insert(
            "x-tilo-contact-handle".into(),
            OwnedValue::from(Str::from("ada@example.test")),
        );
        assert_eq!(parse_badge_count(&hints), Some(9_999));
        assert_eq!(
            parse_contact_endpoint(&hints).as_deref(),
            Some("ada@example.test")
        );
        hints.insert(
            "x-tilo-contact-handle".into(),
            OwnedValue::from(Str::from("bad\nhandle")),
        );
        assert_eq!(parse_contact_endpoint(&hints), None);
    }

    #[test]
    fn sender_name_hint_is_one_trimmed_line() {
        let mut hints = HashMap::new();
        assert_eq!(parse_sender_name(&hints), None);
        hints.insert(
            "x-tilo-sender-name".into(),
            OwnedValue::from(Str::from("  Ada  ")),
        );
        assert_eq!(parse_sender_name(&hints).as_deref(), Some("Ada"));
        hints.insert(
            "x-tilo-sender-name".into(),
            OwnedValue::from(Str::from("Ada\nLovelace")),
        );
        assert_eq!(parse_sender_name(&hints), None);
    }

    #[test]
    fn category_and_suppress_sound_hints_are_read() {
        let mut hints: Hints = HashMap::new();
        assert_eq!(parse_category(&hints), None);
        assert!(!parse_suppress_sound(&hints));
        hints.insert(
            "category".into(),
            OwnedValue::from(Str::from("im.received")),
        );
        assert_eq!(parse_category(&hints).as_deref(), Some("im.received"));
        assert!(!parse_incoming_call(&hints));
        hints.insert("suppress-sound".into(), OwnedValue::from(true));
        assert!(parse_suppress_sound(&hints));
        hints.insert(
            "category".into(),
            OwnedValue::from(Str::from(INCOMING_CALL)),
        );
        assert!(parse_incoming_call(&hints));
        hints.insert("category".into(), OwnedValue::from(Str::from("a\nb")));
        assert_eq!(parse_category(&hints), None);
    }
}
