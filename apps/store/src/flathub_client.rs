//! Flathub JSON API v2 client — the "legacy apps" feed.
//!
//! We only touch the endpoints the store actually renders:
//!
//!   * `GET /collection/popular/last-week/1` — the popular-last-week set.
//!     The v2 API paginates so `/1` is the first page (~30 apps); we may
//!     walk further pages later. `~200` is the ceiling the task asks for,
//!     which the caller enforces via `max_apps`.
//!   * `GET /appstream/<app-id>`             — full AppStream record for
//!     the detail sheet (categories, screenshots, descriptions).
//!
//! Every request runs on the worker thread with a bounded timeout so a
//! flaky Flathub does not stall the UI.

use std::time::Duration;

use reqwest::blocking::Client;
use serde::Deserialize;
use thiserror::Error;

const DEFAULT_TIMEOUT_SECS: u64 = 8;

pub fn api_base() -> String {
    std::env::var("FLATHUB_API_BASE")
        .unwrap_or_else(|_| "https://flathub.org/api/v2".to_string())
}

fn http_timeout() -> Duration {
    std::env::var("QOS_STORE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_TIMEOUT_SECS))
}

#[derive(Debug, Error)]
pub enum FlathubError {
    #[error("http: {0}")]
    Http(String),
    #[error("json decode: {0}")]
    Decode(String),
}

/// One app-id in a popular-collection response.
#[derive(Clone, Debug, Deserialize)]
pub struct PopularEntry {
    #[serde(rename = "id", alias = "flatpakAppId")]
    pub app_id: String,
    /// Full record often embeds a name; treat as optional so the
    /// wire changing doesn't break parsing.
    #[serde(default)]
    pub name: Option<String>,
}

/// Trimmed AppStream record — only the fields the store surfaces.
///
/// The real response has ~30 fields; declaring only the ones we
/// need keeps this stable against Flathub's ongoing shape changes
/// (serde ignores unknown fields by default, but a `serde(deny_unknown_fields)`
/// slip somewhere upstream in a shared crate could still bite us).
#[derive(Clone, Debug, Deserialize)]
pub struct AppStreamRecord {
    #[serde(rename = "flatpakAppId", alias = "id")]
    pub app_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default, rename = "currentReleaseVersion")]
    pub current_release_version: Option<String>,
    #[serde(default)]
    pub categories: Vec<CategoryRef>,
    /// Some payloads carry a `homepage` inline URL; older records
    /// tuck it under a `urls` object. We accept both.
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub urls: Option<UrlsField>,
    /// Optional preview screenshot URLs; we only take the first
    /// image per record so the detail sheet has a hero to show.
    #[serde(default)]
    pub screenshots: Vec<ScreenshotRef>,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct CategoryRef {
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct UrlsField {
    #[serde(default)]
    pub homepage: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ScreenshotRef {
    #[serde(default, rename = "imgDesktopUrl")]
    pub img_desktop_url: Option<String>,
    #[serde(default, rename = "imgMobileUrl")]
    pub img_mobile_url: Option<String>,
}

fn client() -> Result<Client, FlathubError> {
    Client::builder()
        .user_agent("qos-store/0.1 (Flathub)")
        .timeout(http_timeout())
        .build()
        .map_err(|e| FlathubError::Http(e.to_string()))
}

/// Fetch the popular set. `max_apps` bounds the total returned so we
/// don't over-populate the list — the task caps around 200.
pub fn fetch_popular(max_apps: usize) -> Result<Vec<PopularEntry>, FlathubError> {
    let base = api_base();
    let c = client()?;
    let mut out = Vec::new();
    // Walk pages 1..=7 (Flathub returns ~30 per page); stop when we
    // hit the cap or a page comes back empty.
    for page in 1..=7 {
        if out.len() >= max_apps {
            break;
        }
        let url = format!("{}/collection/popular/last-week/{}", base, page);
        let text = c
            .get(&url)
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.text())
            .map_err(|e| FlathubError::Http(e.to_string()))?;
        // Flathub sometimes wraps in `{ "hits": [...] }`, sometimes
        // returns a bare array. Accept both.
        let batch = decode_popular(&text)?;
        if batch.is_empty() {
            break;
        }
        out.extend(batch);
    }
    out.truncate(max_apps);
    Ok(out)
}

/// Fetch one appstream record.
pub fn fetch_appstream(app_id: &str) -> Result<AppStreamRecord, FlathubError> {
    let url = format!("{}/appstream/{}", api_base(), app_id);
    let c = client()?;
    let text = c
        .get(&url)
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.text())
        .map_err(|e| FlathubError::Http(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| FlathubError::Decode(e.to_string()))
}

/// The pure decode: kept a free fn so tests can feed fixtures without
/// standing up an HTTP mock.
pub fn decode_popular(text: &str) -> Result<Vec<PopularEntry>, FlathubError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Bare(Vec<PopularEntry>),
        Hits { hits: Vec<PopularEntry> },
        StringIds(Vec<String>),
        HitStrings { hits: Vec<String> },
    }
    let wire: Wire =
        serde_json::from_str(text).map_err(|e| FlathubError::Decode(e.to_string()))?;
    let out = match wire {
        Wire::Bare(v) => v,
        Wire::Hits { hits } => hits,
        Wire::StringIds(ids) => ids
            .into_iter()
            .map(|id| PopularEntry {
                app_id: id,
                name: None,
            })
            .collect(),
        Wire::HitStrings { hits } => hits
            .into_iter()
            .map(|id| PopularEntry {
                app_id: id,
                name: None,
            })
            .collect(),
    };
    Ok(out)
}

/// Query `flatpak list --app --columns=application,version` on the
/// host and return `(app_id, version)` pairs. Missing binary →
/// empty vec (the caller degrades to "nothing installed").
pub fn installed_flatpaks() -> Vec<(String, String)> {
    let out = match std::process::Command::new("flatpak")
        .args(["list", "--app", "--columns=application,version"])
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => return Vec::new(),
    };
    out.lines()
        .filter_map(|line| {
            let mut cols = line.split('\t');
            let id = cols.next()?.trim();
            let version = cols.next().unwrap_or("").trim();
            if id.is_empty() {
                None
            } else {
                Some((id.to_string(), version.to_string()))
            }
        })
        .collect()
}

/// Whether a usable `flatpak` binary is on PATH.
pub fn flatpak_available() -> bool {
    std::process::Command::new("flatpak")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_bare_array_of_ids() {
        let text = r#"["org.gnome.Calculator","org.videolan.VLC"]"#;
        let out = decode_popular(text).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].app_id, "org.gnome.Calculator");
    }

    #[test]
    fn decode_hits_object_of_objects() {
        let text = r#"{
            "hits": [
                {"id": "org.gnome.Calculator", "name": "Calculator"},
                {"id": "org.videolan.VLC", "name": "VLC"}
            ]
        }"#;
        let out = decode_popular(text).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].app_id, "org.videolan.VLC");
        assert_eq!(out[1].name.as_deref(), Some("VLC"));
    }

    #[test]
    fn decode_hits_object_of_string_ids() {
        let text = r#"{"hits":["org.gnome.Calculator","org.videolan.VLC"]}"#;
        let out = decode_popular(text).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].app_id, "org.gnome.Calculator");
    }
}
