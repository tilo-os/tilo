//! Keys, an input method's composition and the page's focused field on the
//! wire: the `key` and `ime_*` ops the host is told, and its `text_field`
//! and `ime_composition_bounds` reports.

use crate::webview::{KeyInput, WebViewCommand};
use serde_json::{Value, json};
use std::sync::Mutex;
use tilo_lock::MutexRecover as _;

/// The most text one composition carries to the host: an input method
/// composes a phrase, not a document.
pub(super) const IME_COMPOSITION_LIMIT: usize = 4096;

/// A box the host reports in physical view pixels: finite, inside the
/// largest view the host draws, and not empty.
pub(super) fn wire_rect(value: &Value) -> Option<vello::kurbo::Rect> {
    let number = |key: &str| {
        value
            .get(key)
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && v.abs() <= 65536.0)
    };
    let (x, y) = (number("x")?, number("y")?);
    let (width, height) = (number("width")?, number("height")?);
    (width >= 0.0 && height > 0.0).then(|| vello::kurbo::Rect::new(x, y, x + width, y + height))
}

/// The focused field after a host's `text_field` or
/// `ime_composition_bounds` event. Composition bounds belong to the field
/// already focused and are dropped without one.
pub(super) fn text_field_event(
    event_name: &str,
    value: &Value,
    known: Option<crate::webview::TextField>,
) -> Option<crate::webview::TextField> {
    if event_name == "ime_composition_bounds" {
        let mut field = known?;
        field.composition = wire_rect(value);
        return Some(field);
    }
    if value.get("editable").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let purpose = match value.get("purpose").and_then(Value::as_str) {
        Some("password") => crate::ime::InputPurpose::Password,
        Some("email") => crate::ime::InputPurpose::Email,
        Some("url") => crate::ime::InputPurpose::Url,
        Some("phone") => crate::ime::InputPurpose::Phone,
        Some("number") => crate::ime::InputPurpose::Number,
        Some("search") => crate::ime::InputPurpose::Search,
        _ => crate::ime::InputPurpose::Text,
    };
    Some(crate::webview::TextField {
        purpose,
        multiline: value.get("multiline").and_then(Value::as_bool) == Some(true),
        bounds: wire_rect(value),
        // A composition outlives a report of where its field is; the host
        // says when it ends, and the view reads it only while composing.
        composition: known.and_then(|known| known.composition),
    })
}

/// Whether a host report of the focused field or its composition is well
/// formed. A field without a box is one the host cannot place; a box, when
/// given, is whole.
pub(super) fn valid_event(event: &str, value: &Value) -> bool {
    let placed = value.get("x").is_none() || wire_rect(value).is_some();
    if event == "ime_composition_bounds" {
        return placed;
    }
    value["editable"].is_boolean()
        && value
            .get("purpose")
            .is_none_or(|purpose| purpose.as_str().is_some_and(|name| name.len() <= 16))
        && value.get("multiline").is_none_or(Value::is_boolean)
        && placed
}

/// Takes a host report into the engine's focused field; the window places
/// the input method from it on its next frame.
pub(super) fn update(field: &Mutex<Option<crate::webview::TextField>>, event: &str, value: &Value) {
    let mut field = field.lock_recover();
    *field = text_field_event(event, value, *field);
}

/// Whether a key or composition command is within what the host takes.
pub(super) fn within_limits(command: &WebViewCommand) -> bool {
    match command {
        WebViewCommand::Key { key, code, .. } => {
            code.is_none_or(crate::webview::key_code::known)
                && !matches!(key, KeyInput::Character(text) if text.len() > 65536)
        }
        WebViewCommand::ImeComposition { text, .. } => text.len() <= IME_COMPOSITION_LIMIT,
        WebViewCommand::ImeCommit(text) => !text.is_empty() && text.len() <= 65536,
        _ => true,
    }
}

/// The host op for a key or composition command.
pub(super) fn input_command(command: WebViewCommand) -> Value {
    match command {
        WebViewCommand::Key {
            key,
            down,
            modifiers,
            code,
        } => {
            let key = match key {
                KeyInput::Character(value) => value,
                KeyInput::Named(named) => format!("{named:?}"),
            };
            let mut value = json!({"op":"key","key":key,"down":down,"ctrl":modifiers.ctrl,"shift":modifiers.shift,"alt":modifiers.alt,"meta":modifiers.meta});
            if let Some(code) = code {
                value["code"] = json!(code);
            }
            value
        }
        WebViewCommand::ImeComposition { text, cursor } => composition_command(&text, cursor),
        WebViewCommand::ImeCommit(text) => json!({"op":"ime_commit","text":text}),
        WebViewCommand::ImeCancel => json!({"op":"ime_cancel"}),
        _ => json!({"op":"ime_finish"}),
    }
}

/// The `ime_composition` op for `text` with its caret at byte offsets
/// `cursor`: the host counts in UTF-16 code units, as the page does.
pub(super) fn composition_command(text: &str, cursor: Option<(usize, usize)>) -> Value {
    // An offset inside a character counts from that character's start.
    let units = |byte: usize| -> usize {
        text.char_indices()
            .take_while(|&(at, c)| at + c.len_utf8() <= byte)
            .map(|(_, c)| c.len_utf16())
            .sum()
    };
    let (start, end) = cursor.map_or_else(
        || {
            let len = text.encode_utf16().count();
            (len, len)
        },
        |(start, end)| {
            let (start, end) = (units(start), units(end));
            (start.min(end), start.max(end))
        },
    );
    json!({"op":"ime_composition","text":text,"selection_start":start,"selection_end":end})
}

#[cfg(test)]
#[path = "text_field_tests.rs"]
mod tests;
