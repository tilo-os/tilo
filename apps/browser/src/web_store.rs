//! Chrome Web Store items: recognising their pages and fetching their
//! packages from Google's update service for this Chromium version.

use std::io::Read as _;
use std::time::Duration;

use url::Url;

use crate::extensions::ExtensionError;

const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/crx";
/// Matches `extensions::MAX_ARCHIVE_BYTES`.
const MAX_DOWNLOAD_BYTES: u64 = 64 * 1024 * 1024;
/// The Chromium the engine runs when it has not reported its version.
pub const FALLBACK_CHROMIUM_VERSION: &str = "152.0.7977.83";

/// A Web Store listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The extension identifier: thirty-two letters a-p.
    pub id: String,
}

/// The item a Web Store page URL shows, or `None` for any other address.
///
/// Recognised: `https://chromewebstore.google.com/detail/<name>/<id>`, the
/// same without the name, and the legacy
/// `https://chrome.google.com/webstore/detail/<name>/<id>`. Only plain
/// `https` on the default port counts, so a look-alike never offers an install.
pub fn item_from_url(address: &str) -> Option<Item> {
    let url = Url::parse(address).ok()?;
    if url.scheme() != "https"
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.collect();
    let detail = match url.host_str()? {
        "chromewebstore.google.com" => segments.as_slice(),
        "chrome.google.com" => segments.strip_prefix(&["webstore"])?,
        _ => return None,
    };
    let id = match detail {
        ["detail", id] | ["detail", _, id] => *id,
        _ => return None,
    };
    crate::crx::is_extension_id(id).then(|| Item { id: id.to_owned() })
}

/// The update-service address of the CRX3 package for `id` that Chromium
/// `chromium_version` would be offered, on this platform.
pub fn crx_url(id: &str, chromium_version: &str) -> Option<Url> {
    if !crate::crx::is_extension_id(id)
        || chromium_version.is_empty()
        || !chromium_version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return None;
    }
    let (arch, nacl) = match std::env::consts::ARCH {
        "aarch64" => ("arm64", "arm"),
        _ => ("x64", "x86-64"),
    };
    let mut url = Url::parse(UPDATE_SERVICE).ok()?;
    url.query_pairs_mut()
        .append_pair("response", "redirect")
        .append_pair("os", "linux")
        .append_pair("arch", arch)
        .append_pair("os_arch", arch)
        .append_pair("nacl_arch", nacl)
        .append_pair("prod", "chromecrx")
        .append_pair("prodchannel", "")
        .append_pair("prodversion", chromium_version)
        .append_pair("lang", "en-US")
        .append_pair("acceptformat", "crx3")
        .append_pair("x", &format!("id={id}&installsource=ondemand&uc"));
    Some(url)
}

/// Whether a download may come from `host`: Google's update service and the
/// content hosts it redirects to.
fn trusted_host(host: &str) -> bool {
    [".google.com", ".googleusercontent.com", ".gvt1.com"]
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// Fetch the package for `id`. Blocking; call from a worker thread.
///
/// # Errors
///
/// Returns [`ExtensionError::Download`] when the address is invalid, the
/// service redirects off Google's hosts, the request fails, or the package
/// exceeds the size limit. The package is not verified here.
pub fn download(id: &str, chromium_version: &str) -> Result<Vec<u8>, ExtensionError> {
    let url = crx_url(id, chromium_version)
        .ok_or_else(|| ExtensionError::Download("invalid extension identifier".into()))?;
    let client = tilo_net::http::blocking_client()
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let allowed = attempt.url().scheme() == "https"
                && attempt.url().host_str().is_some_and(trusted_host)
                && attempt.previous().len() < 5;
            if allowed {
                attempt.follow()
            } else {
                attempt.error("redirect leaves the Chrome Web Store")
            }
        }))
        .build()
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_DOWNLOAD_BYTES)
    {
        return Err(ExtensionError::Download("package is too large".into()));
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_DOWNLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(ExtensionError::Download("package is too large".into()));
    }
    if bytes.is_empty() {
        // The service answers 204 for an unknown or unavailable item.
        return Err(ExtensionError::Download(
            "the Web Store has no package for this extension".into(),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONEPASSWORD: &str = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
    const UBLOCK_LITE: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";

    #[test]
    fn store_pages_are_recognised_and_lookalikes_are_not() {
        let id = |url: &str| item_from_url(url).map(|item| item.id);
        // phase: current_and_legacy_item_pages
        assert_eq!(
            id(&format!(
                "https://chromewebstore.google.com/detail/1password/{ONEPASSWORD}?hl=en"
            )),
            Some(ONEPASSWORD.into())
        );
        assert_eq!(
            id(&format!(
                "https://chromewebstore.google.com/detail/{UBLOCK_LITE}#reviews"
            )),
            Some(UBLOCK_LITE.into())
        );
        assert_eq!(
            id(&format!(
                "https://chrome.google.com/webstore/detail/ublock-origin-lite/{UBLOCK_LITE}"
            )),
            Some(UBLOCK_LITE.into())
        );
        // phase: other_pages_and_hosts_are_not_items
        for rejected in [
            format!("http://chromewebstore.google.com/detail/x/{ONEPASSWORD}"),
            format!("https://chromewebstore.google.com.evil.example/detail/x/{ONEPASSWORD}"),
            format!("https://user@chromewebstore.google.com/detail/x/{ONEPASSWORD}"),
            format!("https://chromewebstore.google.com:8443/detail/x/{ONEPASSWORD}"),
            format!("https://chrome.google.com/detail/x/{ONEPASSWORD}"),
            "https://chromewebstore.google.com/category/extensions".to_owned(),
            "https://chromewebstore.google.com/detail/1password/short".to_owned(),
            format!("https://chromewebstore.google.com/detail/a/b/{ONEPASSWORD}"),
            format!("https://example.org/detail/x/{ONEPASSWORD}"),
            "not a url".to_owned(),
        ] {
            assert_eq!(id(&rejected), None, "{rejected}");
        }
    }

    #[test]
    fn package_address_carries_the_engine_version() {
        let url = crx_url(ONEPASSWORD, "152.0.7977.83").unwrap();
        assert_eq!(url.host_str(), Some("clients2.google.com"));
        assert_eq!(url.path(), "/service/update2/crx");
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["prodversion"], "152.0.7977.83");
        assert_eq!(query["acceptformat"], "crx3");
        assert_eq!(query["response"], "redirect");
        assert_eq!(
            query["x"],
            format!("id={ONEPASSWORD}&installsource=ondemand&uc")
        );
        assert!(crx_url("nope", "152.0.7977.83").is_none());
        assert!(crx_url(ONEPASSWORD, "152;rm").is_none());
        assert!(crx_url(ONEPASSWORD, "").is_none());
        assert!(trusted_host("clients2.google.com"));
        assert!(trusted_host("clients2.googleusercontent.com"));
        assert!(!trusted_host("evilgoogle.com"));
        assert!(!trusted_host("google.com.evil.example"));
    }
}
