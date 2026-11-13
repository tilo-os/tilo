//! Chrome Web Store items: recognising their pages and fetching their packages.

use std::io::Read as _;
use std::time::Duration;

use url::Url;

use crate::extensions::ExtensionError;

const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/crx";
/// Matches `extensions::MAX_ARCHIVE_BYTES`.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
pub const FALLBACK_CHROMIUM_VERSION: &str = "152.0.7977.83";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The extension id: thirty-two letters a-p.
    pub id: String,
}

/// The item the page at `address` shows; `None` for any other address.
///
/// Known: `https://chromewebstore.google.com/detail/<name>/<id>`, the same
/// without the name, and the legacy `chrome.google.com/webstore/detail/...`.
/// Only plain `https` on the default port counts, so a look-alike page never
/// offers an install.
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

pub fn crx_url(id: &str, chromium_version: &str) -> Option<Url> {
    crx_url_from(id, None, chromium_version)
}

fn crx_url_from(id: &str, installed: Option<&str>, chromium_version: &str) -> Option<Url> {
    if !crate::crx::is_extension_id(id)
        || installed.is_some_and(|version| {
            version.is_empty()
                || !version
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.')
        })
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
        .append_pair(
            "x",
            &match installed {
                Some(version) => format!("id={id}&v={version}&installsource=ondemand&uc"),
                None => format!("id={id}&installsource=ondemand&uc"),
            },
        );
    Some(url)
}

fn trusted_host(host: &str) -> bool {
    [".google.com", ".googleusercontent.com", ".gvt1.com"]
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// Fetch the package for `id`. Blocking; call from a worker thread. The
/// package is not verified here.
///
/// # Errors
///
/// [`ExtensionError::Download`] when the address or the request fails or the
/// service redirects off Google's hosts, [`ExtensionError::NoPackage`] when
/// the store has none for this browser, and [`ExtensionError::TooLarge`] when
/// the package exceeds the size limit.
pub fn download(id: &str, chromium_version: &str) -> Result<Vec<u8>, ExtensionError> {
    fetch(id, None, chromium_version)?.ok_or(ExtensionError::NoPackage)
}

/// The package of a newer version of `id` than `installed`, or `None`.
/// Blocking; call from a worker thread.
///
/// # Errors
/// As [`download`].
pub fn update(
    id: &str,
    installed: &str,
    chromium_version: &str,
) -> Result<Option<Vec<u8>>, ExtensionError> {
    fetch(id, Some(installed), chromium_version)
}

fn fetch(
    id: &str,
    installed: Option<&str>,
    chromium_version: &str,
) -> Result<Option<Vec<u8>>, ExtensionError> {
    let url = crx_url_from(id, installed, chromium_version)
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
        return Err(ExtensionError::TooLarge);
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_DOWNLOAD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ExtensionError::Download(error.to_string()))?;
    if bytes.len() as u64 > MAX_DOWNLOAD_BYTES {
        return Err(ExtensionError::TooLarge);
    }
    // The service answers 204, nothing, for no package: an item with none
    // for this browser, or none newer than the one installed.
    Ok((!bytes.is_empty()).then_some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONEPASSWORD: &str = "aeblfdkhhhdcdjpifhhbdiojplfjncoa";
    const UBLOCK_LITE: &str = "ddkjiahejlhfcafbddmgiahcphecmpfh";

    #[test]
    fn store_pages_are_recognised_and_lookalikes_are_not() {
        let id = |url: &str| item_from_url(url).map(|item| item.id);
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
