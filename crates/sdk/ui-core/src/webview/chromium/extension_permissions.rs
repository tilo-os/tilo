//! An extension's request for more access (`chrome.permissions.request`),
//! and what it holds of what it may ask for, as the host tells the Browser
//! of them (`extension_permissions.h`).

use serde_json::Value;

use crate::webview::{ExtensionPermissionSet, WebViewEvent};

/// An extension's request for more access, or what it holds of what it may
/// ask for now: a set of API permissions and site patterns, each bounded.
pub(super) fn decode(value: &Value) -> Option<WebViewEvent> {
    let id = value["id"]
        .as_str()
        .filter(|id| super::extension_id(id))?
        .to_owned();
    let list = |key: &str| -> Option<Vec<String>> {
        let entries = value[key]
            .as_array()
            .filter(|entries| entries.len() <= 512)?;
        entries
            .iter()
            .map(|entry| {
                entry
                    .as_str()
                    .filter(|entry| entry.len() <= 2048)
                    .map(str::to_owned)
            })
            .collect()
    };
    let set = ExtensionPermissionSet {
        permissions: list("permissions")?,
        origins: list("origins")?,
    };
    if value["event"] == "extension_permissions" {
        return Some(WebViewEvent::ExtensionPermissions { id, granted: set });
    }
    let request = value["request"]
        .as_u64()
        .filter(|request| *request > 0)
        .and_then(|request| u32::try_from(request).ok())
        .filter(|request| i32::try_from(*request).is_ok())?;
    (!set.is_empty()).then_some(WebViewEvent::ExtensionPermissionRequest {
        request,
        id,
        asked: set,
    })
}
