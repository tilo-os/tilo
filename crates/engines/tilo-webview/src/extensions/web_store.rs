//! Chrome Web Store items: recognising their pages and addresses, and fetching their packages
//! from the store's update service.
//!
//! The store's pages show an item; its package comes from the same service a browser installs
//! and updates its extensions with. Only plain `https` on the store's own hosts counts, and
//! every redirect stays among Google's download hosts, so a look-alike page never installs
//! anything.

use std::io::Read as _;
use std::time::Duration;

use url::Url;

use super::ExtensionError;

const UPDATE_SERVICE: &str = "https://clients2.google.com/service/update2/crx";
/// Matches the size an extension package may be.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;
/// What the update service is told the browser is when the host has not said its version yet.
pub const FALLBACK_CHROMIUM_VERSION: &str = "152.0.7977.83";

/// One item of the store: the extension id its page names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The extension id: thirty-two letters a-p.
    pub id: String,
}

/// The item `said` names: a store page's address, or the extension id itself.
///
/// Known pages: `https://chromewebstore.google.com/detail/<name>/<id>`, the same without the
/// name, and the legacy `chrome.google.com/webstore/detail/...`.
#[must_use]
pub fn item(said: &str) -> Option<Item> {
    let said = said.trim();
    if super::crx::is_extension_id(said) {
        return Some(Item {
            id: said.to_owned(),
        });
    }
    item_from_url(said)
}

/// The item the page at `address` shows; `None` for any other address.
///
/// Only plain `https` on the default port counts, so a look-alike page never offers an install.
#[must_use]
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
    super::crx::is_extension_id(id).then(|| Item { id: id.to_owned() })
}

/// The address the package for `id` is fetched from, the update service's own: with `installed`
/// it answers a newer version than that one.
#[must_use]
pub fn crx_url(id: &str, installed: Option<&str>, chromium_version: &str) -> Option<Url> {
    if !super::crx::is_extension_id(id)
        || installed.is_some_and(|version| !is_version(version))
        || !is_version(chromium_version)
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

fn is_version(version: &str) -> bool {
    !version.is_empty()
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

fn trusted_host(host: &str) -> bool {
    [".google.com", ".googleusercontent.com", ".gvt1.com"]
        .iter()
        .any(|suffix| host.ends_with(suffix))
}

/// The package for `id`, or a newer one than `installed`. Blocking; call from a worker thread.
/// The package's own proofs are not verified here.
///
/// # Errors
///
/// [`ExtensionError::Download`] when the address or the request fails or the service redirects
/// off Google's hosts, and [`ExtensionError::TooLarge`] when the package exceeds the size limit.
pub fn fetch(
    id: &str,
    installed: Option<&str>,
    chromium_version: &str,
) -> Result<Option<Vec<u8>>, ExtensionError> {
    let url = crx_url(id, installed, chromium_version)
        .ok_or_else(|| ExtensionError::Download("invalid extension identifier".to_owned()))?;
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
    // The service answers 204, nothing, for no package: an item with none for this browser, or
    // none newer than the one installed.
    Ok((!bytes.is_empty()).then_some(bytes))
}
