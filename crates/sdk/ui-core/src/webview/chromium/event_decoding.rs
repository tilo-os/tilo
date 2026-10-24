//! Decoding the host's events that carry what a page, a site or an
//! extension said: each is taken only when every part of it is what the
//! host sends.

use super::{
    FIRST_POPUP, MAX_PAGE_URL, Value, WebViewEvent, audio, decode_base64_bounded,
    decode_extension_menu, extension_id, extension_notification, extension_omnibox,
    extension_permissions, extension_sign_in, page_clipboard, text_field, valid_context_menu,
    valid_id, wire_u64,
};

pub(super) fn valid_site_event(value: &Value, event: &str) -> bool {
    let header = value["request_id"]
        .as_u64()
        .is_some_and(|n| n > 0 && i32::try_from(n).is_ok())
        && value["origin"].as_str().is_some_and(|s| s.len() <= 8192)
        && value["generation"].as_f64().is_some_and(|n| {
            n.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0
        });
    header
        && if event == "site_result" {
            value["success"].is_boolean()
                && value["action"].as_str().is_some_and(|s| {
                    matches!(
                        s,
                        "status" | "clear" | "revoke" | "reset" | "enable" | "disable" | "reload"
                    )
                })
                && value["message"].as_str().is_some_and(|s| s.len() <= 1024)
        } else {
            ["global_enabled", "excepted", "effective"]
                .iter()
                .all(|k| value[*k].is_boolean())
                && value["blocked"].as_f64().is_some_and(|n| {
                    n.is_finite()
                        && (0.0..=9_007_199_254_740_991.0).contains(&n)
                        && n.fract() == 0.0
                })
                && matches!(
                    value["security"].as_str(),
                    Some("insecure" | "secure" | "unverified")
                )
                && ["subject", "issuer"]
                    .iter()
                    .all(|k| value[*k].as_str().is_some_and(|s| s.len() <= 512))
                && ["certificate_status", "tls_version", "content_status"]
                    .iter()
                    .all(|k| value[*k].as_u64().is_some_and(|n| u32::try_from(n).is_ok()))
                && value["expires"].as_f64().is_some_and(f64::is_finite)
                && value["permissions"].as_object().is_some_and(|p| {
                    p.len() == 4
                        && ["camera", "microphone", "location", "notifications"]
                            .iter()
                            .all(|k| {
                                matches!(
                                    p.get(*k).and_then(Value::as_str),
                                    Some("allowed" | "blocked" | "ask")
                                )
                            })
                })
        }
}

/// An `extension_action`, `extension_open_popup`, `extension_execute_action`,
/// `extension_notification`, `extension_sign_in`, `extension_omnibox_*` or
/// `extension_side_panel`
/// event, when every part of it is what the host sends: the extension's id,
/// its own pages or a web page to sign in on, bounded text and an icon of at
/// most 128 pixels a side.
pub(super) fn decode_extension_event(value: &Value) -> Option<WebViewEvent> {
    // Absent: unchanged. Null: taken back. Otherwise a value `read` accepts,
    // or the event is refused.
    macro_rules! field {
        ($change:ident, $key:literal, $read:expr) => {
            match $change.get($key) {
                None => None,
                Some(Value::Null) => Some(None),
                Some(value) => Some(Some($read(value)?)),
            }
        };
    }
    let id = value["id"].as_str().filter(|id| extension_id(id))?;
    let own = |url: &str| {
        url.len() <= 2048
            && url
                .strip_prefix("chrome-extension://")
                .and_then(|rest| rest.strip_prefix(id))
                .is_some_and(|rest| rest.starts_with('/'))
    };
    if value["event"] == "extension_execute_action" {
        return Some(WebViewEvent::ExtensionExecuteAction { id: id.into() });
    }
    if value["event"] == "extension_notification" {
        return extension_notification::decode(id, value);
    }
    if value["event"] == "extension_sign_in" {
        return extension_sign_in::decode(id, value);
    }
    if value["event"]
        .as_str()
        .is_some_and(|event| event.starts_with("extension_omnibox_"))
    {
        return extension_omnibox::decode(id, value);
    }
    if value["event"] == "extension_open_popup" {
        let url = value["url"].as_str().filter(|url| own(url))?;
        return Some(WebViewEvent::ExtensionOpenPopup {
            id: id.into(),
            url: url.into(),
        });
    }
    if value["event"] == "extension_side_panel" {
        use crate::webview::ExtensionSidePanelChange as Change;
        let url = match &value["url"] {
            Value::Null => None,
            url => Some(url.as_str().filter(|url| own(url))?.to_owned()),
        };
        let change = match (value["mode"].as_str()?, url) {
            ("open", Some(url)) => Change::Open(url),
            ("toggle", Some(url)) => Change::Toggle(url),
            ("options", url) => Change::Options(url),
            ("close", None) => Change::Close,
            _ => return None,
        };
        return Some(WebViewEvent::ExtensionSidePanel {
            id: id.into(),
            change,
        });
    }
    let change = value["change"].as_object()?;
    let text = |value: &Value| {
        value
            .as_str()
            .filter(|text| text.len() <= 1024)
            .map(str::to_owned)
    };
    let icon = |value: &Value| {
        if let Some(path) = value["path"].as_str() {
            return (path.starts_with('/') && path.len() <= 2048)
                .then(|| crate::webview::ExtensionActionIcon::Path(path.into()));
        }
        let side = |key: &str| {
            u32::try_from(value[key].as_u64()?)
                .ok()
                .filter(|side| (1..=128).contains(side))
        };
        let (width, height) = (side("width")?, side("height")?);
        let length = usize::try_from(width.checked_mul(height)?.checked_mul(4)?).ok()?;
        let rgba = decode_base64_bounded(value["data"].as_str()?, length)?;
        (rgba.len() == length).then_some(crate::webview::ExtensionActionIcon::Pixels {
            width,
            height,
            rgba,
        })
    };
    let popup = |value: &Value| {
        value
            .as_str()
            .filter(|url| url.is_empty() || own(url))
            .map(str::to_owned)
    };
    if change.keys().any(|key| {
        !matches!(
            key.as_str(),
            "badge" | "title" | "popup" | "enabled" | "icon"
        )
    }) {
        return None;
    }
    Some(WebViewEvent::ExtensionAction {
        id: id.into(),
        global: value["global"].as_bool()?,
        change: crate::webview::ExtensionActionChange {
            badge: field!(change, "badge", text),
            title: field!(change, "title", text),
            popup: field!(change, "popup", popup),
            enabled: field!(change, "enabled", Value::as_bool),
            icon: field!(change, "icon", icon),
        },
    })
}

pub(super) fn decode_site_event(value: &Value, event: &str) -> Option<WebViewEvent> {
    Some(match event {
        "site_status" => WebViewEvent::SiteStatus(crate::webview::SiteStatus {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            blocked: wire_u64(&value["blocked"])?,
            global_enabled: value["global_enabled"].as_bool()?,
            excepted: value["excepted"].as_bool()?,
            effective: value["effective"].as_bool()?,
            security: value["security"].as_str()?.into(),
            certificate_status: u32::try_from(value["certificate_status"].as_u64()?).ok()?,
            tls_version: u32::try_from(value["tls_version"].as_u64()?).ok()?,
            content_status: u32::try_from(value["content_status"].as_u64()?).ok()?,
            subject: value["subject"].as_str()?.into(),
            issuer: value["issuer"].as_str()?.into(),
            expires: value["expires"].as_f64()?,
            permissions: value["permissions"]
                .as_object()?
                .iter()
                .map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
                .collect::<Option<Vec<_>>>()?,
            can_reset: value["can_reset"].as_bool().unwrap_or(false),
        }),
        "site_result" => WebViewEvent::SiteResult {
            request_id: u32::try_from(value["request_id"].as_u64()?).ok()?,
            origin: value["origin"].as_str()?.into(),
            generation: wire_u64(&value["generation"])?,
            action: value["action"].as_str()?.into(),
            success: value["success"].as_bool()?,
            message: value["message"].as_str()?.into(),
        },
        _ => return None,
    })
}

/// The longest text of a page's question the host sends, in bytes.
const MAX_PAGE_DIALOG_TEXT: usize = 16 * 1024;

/// A `js_dialog` event: the page's question, its texts within the host's
/// bounds.
pub(super) fn decode_page_dialog(value: &Value) -> Option<crate::webview::PageDialog> {
    let text = |key: &str| {
        value[key]
            .as_str()
            .filter(|text| text.len() <= MAX_PAGE_DIALOG_TEXT)
            .map(str::to_owned)
    };
    let kind = match value["kind"].as_str()? {
        "alert" => crate::webview::PageDialogKind::Alert,
        "confirm" => crate::webview::PageDialogKind::Confirm,
        "prompt" => crate::webview::PageDialogKind::Prompt,
        "beforeunload" => crate::webview::PageDialogKind::Leave {
            reload: value["reload"].as_bool()?,
        },
        _ => return None,
    };
    Some(crate::webview::PageDialog {
        id: u32::try_from(value["id"].as_u64()?)
            .ok()
            .filter(|_| valid_id(value))?,
        kind,
        origin: value["origin"]
            .as_str()
            .filter(|origin| origin.len() <= 8192)?
            .to_owned(),
        message: text("message")?,
        default_text: text("default_text")?,
    })
}

/// An `extension_shortcuts` event: the keys extensions' commands hold, each
/// a key as Chrome names it with its modifiers, at most 256.
pub(super) fn decode_extension_shortcuts(
    value: &Value,
) -> Option<Vec<crate::webview::ExtensionShortcut>> {
    const NAMED: [&str; 13] = [
        "Comma", "Period", "Space", "Home", "End", "PageUp", "PageDown", "Delete", "Tab", "Up",
        "Down", "Left", "Right",
    ];
    let list = value["shortcuts"]
        .as_array()
        .filter(|list| list.len() <= 256)?;
    list.iter()
        .map(|entry| {
            let key = entry["key"].as_str().filter(|key| {
                (key.len() == 1
                    && key
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
                    || NAMED.contains(key)
            })?;
            let flag = |name: &str| entry[name].as_bool();
            Some(crate::webview::ExtensionShortcut {
                key: key.to_owned(),
                modifiers: crate::webview::KeyModifiers {
                    ctrl: flag("ctrl")?,
                    shift: flag("shift")?,
                    alt: flag("alt")?,
                    meta: flag("meta")?,
                },
            })
        })
        .collect()
}

/// Whether a host event is well formed: an event this engine knows is
/// taken only when every part of it is what the host sends.
pub(super) fn validate_event(value: &Value) -> Result<(), &'static str> {
    if value.get("tab").is_some_and(|tab| {
        !tab.as_u64()
            .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
    }) {
        return Err("Invalid tab ID");
    }
    let event = value
        .get("event")
        .and_then(Value::as_str)
        .ok_or("missing event name")?;
    let text = |key: &str| value.get(key).is_some_and(Value::is_string);
    let valid = match event {
        "credential_result" => crate::webview::CredentialReply::decode(value).is_some(),
        "service_session_result" => crate::webview::ServiceSessionReply::decode(value).is_some(),
        "site_status" | "site_result" => valid_site_event(value, event),
        "extension_action"
        | "extension_open_popup"
        | "extension_execute_action"
        | "extension_notification"
        | "extension_sign_in"
        | "extension_omnibox_suggestions"
        | "extension_omnibox_default"
        | "extension_side_panel" => decode_extension_event(value).is_some(),
        "extension_menu" => decode_extension_menu(value).is_some(),
        "extension_shortcuts" => decode_extension_shortcuts(value).is_some(),
        "extension_permission_request" | "extension_permissions" => {
            extension_permissions::decode(value).is_some()
        }
        "devtools_result" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["ok"].is_boolean()
                && value["body"]
                    .as_str()
                    .is_some_and(|body| body.len() <= 1024 * 1024)
        }
        "context_closed" | "permission_closed" | "js_dialog_closed" => valid_id(value),
        "js_dialog" => decode_page_dialog(value).is_some(),
        "context_menu" => valid_context_menu(value),
        "find_result" => {
            valid_id(value)
                && ["count", "active"].iter().all(|key| {
                    value[*key]
                        .as_i64()
                        .is_some_and(|n| (-1..=i64::from(i32::MAX)).contains(&n))
                })
                && value["final"].is_boolean()
        }
        "zoom" => value["level"]
            .as_f64()
            .is_some_and(|level| level.is_finite() && (-5.0..=9.0).contains(&level)),
        "fullscreen" => value["fullscreen"].is_boolean(),
        "audio_start" | "audio" | "audio_stop" => audio::valid_event(event, value),
        "renderer_terminated" => value
            .as_object()
            .is_some_and(|object| object.len() == if value.get("tab").is_some() { 2 } else { 1 }),
        "permission_prompt" => valid_id(value) && text("origin") && text("permissions"),
        "download_prompt" => valid_id(value) && text("url") && text("name"),
        "download" => {
            valid_id(value)
                && text("path")
                && text("name")
                // Any length: a page's own export (a `data:` address) is
                // its whole file. What is kept of it is bounded in decoding.
                && text("url")
                && value["retryable"].is_boolean()
                && text("error")
                && matches!(
                    value["state"].as_str(),
                    Some("progress" | "complete" | "cancelled" | "failed")
                )
                && wire_u64(&value["received"]).is_some()
                && wire_u64(&value["total"]).is_some()
        }
        "download_runtime" => {
            ["admitted", "detached"]
                .iter()
                .all(|key| value[*key].as_u64().is_some_and(|count| count <= 32))
                && value["accepting"].is_boolean()
                && value["error"]
                    .as_str()
                    .is_some_and(|error| error.len() <= 1024)
        }
        "ready" => {
            value
                .get("native_caret_text_input")
                .is_none_or(Value::is_boolean)
                && text("cef_version")
                && text("chromium_version")
                && value
                    .get("guarded_edit_history")
                    .is_none_or(Value::is_boolean)
                && value
                    .get("native_edit_history")
                    .is_none_or(Value::is_boolean)
                && value.get("document_size").is_none_or(Value::is_boolean)
                && value.get("selection_text").is_none_or(Value::is_boolean)
                && value.get("page_clipboard").is_none_or(Value::is_boolean)
                && value.get("sealed_documents").is_none_or(Value::is_boolean)
        }
        "tab_ready" => {
            value
                .get("native_caret_text_input")
                .is_none_or(Value::is_boolean)
                && value
                    .get("guarded_edit_history")
                    .is_none_or(Value::is_boolean)
                && value
                    .get("native_edit_history")
                    .is_none_or(Value::is_boolean)
                && value.get("document_size").is_none_or(Value::is_boolean)
        }
        "clipboard" | "clipboard_read" => page_clipboard::decode(value).is_some(),
        "selection_text" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["text"]
                    .as_str()
                    .is_some_and(|text| text.len() <= 1024 * 1024)
        }
        "link_status" => value["url"].as_str().is_some_and(|url| url.len() <= 8192),
        "extension_download_open" => valid_id(value) && value["reveal"].is_boolean(),
        "find_selection" => {
            valid_id(value)
                && ["x", "y", "width", "height"].iter().all(|key| {
                    value[*key]
                        .as_f64()
                        .is_some_and(|n| n.is_finite() && (-1e7..=1e7).contains(&n))
                })
        }
        "document_size" => ["width", "height", "scroll_y"].iter().all(|key| {
            value[*key]
                .as_f64()
                .is_some_and(|n| n.is_finite() && (0.0..=1e7).contains(&n))
        }),
        "open_tab" => {
            value["url"].as_str().is_some_and(|url| url.len() <= 8192)
                && value["background"].is_boolean()
        }
        "popup" => {
            value["popup"]
                .as_u64()
                .is_some_and(|id| id >= u64::from(FIRST_POPUP) && i32::try_from(id).is_ok())
                && value["url"].as_str().is_some_and(|url| url.len() <= 8192)
                && ["width", "height"]
                    .iter()
                    .all(|key| value[*key].as_u64().is_some_and(|n| n <= 10_000))
        }
        "cursor" => value["cursor"]
            .as_str()
            .is_some_and(|name| !name.is_empty() && name.len() <= 64),
        "uri" => text("url"),
        "title" => text("title"),
        "progress" => value["progress"]
            .as_f64()
            .is_some_and(|n| n.is_finite() && (0.0..=1.0).contains(&n)),
        "loaded" => text("url") && value["status"].as_i64().is_some(),
        "navigation_outcome" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                // The frame's address, as long as Chromium lets one be.
                && value["url"]
                    .as_str()
                    .is_some_and(|uri| uri.len() <= MAX_PAGE_URL)
                && value.get("error").is_none_or(|error| {
                    error
                        .as_str()
                        .is_some_and(|message| !message.is_empty() && message.len() <= 4096)
                })
        }
        "navigation_history" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && crate::webview::NavigationDirection::from_wire(value["direction"].as_str())
                    .is_some()
                && value["entries"].as_array().is_some_and(|entries| {
                    entries.len() <= 12
                        && entries.iter().all(|entry| {
                            valid_id(entry)
                                && entry["url"].as_str().is_some_and(|s| s.len() <= 8192)
                                && entry["title"].as_str().is_some_and(|s| s.len() <= 4096)
                        })
                })
        }
        "history" => value["back"].is_boolean() && value["forward"].is_boolean(),
        "text_field" | "ime_composition_bounds" => text_field::valid_event(event, value),
        "page_result" | "text_input_result" => crate::webview::page::decode_result(value).is_some(),
        "paint_snapshot" | "paint_resource" => {
            value["request_id"]
                .as_u64()
                .is_some_and(|id| id > 0 && i32::try_from(id).is_ok())
                && value["ok"].is_boolean()
        }
        // A load's failure names its address; the engine's own trouble (a
        // filter that could not check a resource) does not.
        "error" => value.get("url").is_none_or(Value::is_string) && text("message"),
        _ => true,
    };
    if valid {
        Ok(())
    } else {
        Err("malformed known event")
    }
}
