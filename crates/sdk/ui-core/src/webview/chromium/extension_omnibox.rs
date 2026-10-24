//! What the person types after an extension's keyword in the Browser's
//! address (`chrome.omnibox`), as the engine is told of it, and the
//! extension's suggestions as the host tells the Browser of them
//! (`extension_tabs.h`).

use serde_json::{Value, json};

use crate::webview::{OmniboxInput, OmniboxSuggestion, WebViewEvent};

/// The most suggestions the host sends for an extension's keyword at once,
/// and the longest text of one or of what is typed after the keyword.
const SUGGESTIONS: usize = 16;
const TEXT_BYTES: usize = 2048;

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| text.len() <= TEXT_BYTES)
        .map(str::to_owned)
}

/// Extension `id`'s `extension_omnibox_suggestions` or
/// `extension_omnibox_default` event, when every part of it is what the
/// host sends; `None` for any other event.
pub(super) fn decode(id: &str, value: &Value) -> Option<WebViewEvent> {
    match value["event"].as_str()? {
        "extension_omnibox_suggestions" => {
            let suggestions = value["suggestions"]
                .as_array()
                .filter(|list| list.len() <= SUGGESTIONS)?
                .iter()
                .map(|suggestion| {
                    Some(OmniboxSuggestion {
                        content: text(&suggestion["content"])?,
                        description: text(&suggestion["description"])?,
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(WebViewEvent::ExtensionOmniboxSuggestions {
                id: id.into(),
                request: u32::try_from(value["request"].as_u64()?).ok()?,
                suggestions,
            })
        }
        "extension_omnibox_default" => {
            let description = match value.get("description")? {
                Value::Null => None,
                description => Some(text(description)?),
            };
            Some(WebViewEvent::ExtensionOmniboxDefault {
                id: id.into(),
                description,
            })
        }
        _ => None,
    }
}

/// Whether what is typed is what the host reads: bounded text, and a
/// request within the numbers it carries.
pub(super) fn within_limits(input: &OmniboxInput) -> bool {
    match input {
        OmniboxInput::Changed { text, request } => {
            text.len() <= TEXT_BYTES && i32::try_from(*request).is_ok()
        }
        OmniboxInput::Entered { text } => text.len() <= TEXT_BYTES,
        OmniboxInput::Started | OmniboxInput::Cancelled => true,
    }
}

/// The host's `extension_omnibox` command for extension `id`.
pub(super) fn encode(id: &str, input: OmniboxInput) -> Value {
    match input {
        OmniboxInput::Started => json!({"op":"extension_omnibox","id":id,"input":"started"}),
        OmniboxInput::Changed { text, request } => {
            json!({"op":"extension_omnibox","id":id,"input":"changed","text":text,"request":request})
        }
        OmniboxInput::Entered { text } => {
            json!({"op":"extension_omnibox","id":id,"input":"entered","text":text})
        }
        OmniboxInput::Cancelled => json!({"op":"extension_omnibox","id":id,"input":"cancelled"}),
    }
}
