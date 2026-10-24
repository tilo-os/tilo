//! An extension asking the person to sign in on a web page
//! (`identity.launchWebAuthFlow`), as the host tells the Browser of it
//! (`extension_sign_in.h`).

use serde_json::Value;

use crate::webview::WebViewEvent;

/// Extension `id`'s sign-in, when its page is a web page of a bounded
/// length and the host numbered it as it numbers tabs' requests.
pub(super) fn decode(id: &str, value: &Value) -> Option<WebViewEvent> {
    let url = value["url"].as_str().filter(|url| {
        url.len() <= 8192
            && url::Url::parse(url).is_ok_and(|parsed| {
                matches!(parsed.scheme(), "http" | "https") && parsed.host().is_some()
            })
    })?;
    Some(WebViewEvent::ExtensionSignIn {
        id: id.into(),
        url: url.into(),
        interactive: value["interactive"].as_bool()?,
        flow: u32::try_from(value["flow"].as_u64()?)
            .ok()
            .filter(|flow| *flow > 0 && i32::try_from(*flow).is_ok())?,
    })
}
