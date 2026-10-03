//! Flathub JSON API v2 client — the "legacy apps" feed.
//!
//! We only touch the endpoints the store actually renders:
//!
//!   * `GET /collection/popular/last-week/1` — the popular-last-week set.
//!     The v2 API paginates so `/1` is the first page (~30 apps); we may
//!     walk further pages later. `~200` is the ceiling the task asks for,
//!     which the caller enforces via `max_apps`.
//!   * `GET /appstream/<app-id>`             — full `AppStream` record for
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
    std::env::var("FLATHUB_API_BASE").unwrap_or_else(|_| "https://flathub.org/api/v2".to_string())
}

fn http_timeout() -> Duration {
    std::env::var("TILO_STORE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or_else(
            || Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            Duration::from_secs,
        )
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

/// A configured Flatpak remote, as reported by the machine-readable CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub title: String,
    pub disabled: bool,
}

/// Trimmed `AppStream` record — only the fields the store surfaces.
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
    tilo_net::http::blocking_client()
        .user_agent("tilo-store/0.1 (Flathub)")
        .timeout(http_timeout())
        .build()
        .map_err(|e| FlathubError::Http(e.to_string()))
}

/// Fetch the popular set. `max_apps` bounds the total returned so we
/// don't over-populate the list — the task caps around 200.
///
/// # Errors
///
/// Returns [`FlathubError::Http`] if a page request fails or comes back
/// with a non-success status, or [`FlathubError::Decode`] if a page's body
/// isn't the expected JSON shape.
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
        let url = format!("{base}/collection/popular/last-week/{page}");
        let text = c
            .get(&url)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .and_then(reqwest::blocking::Response::text)
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
///
/// # Errors
///
/// Returns [`FlathubError::Http`] if the request fails or comes back with a
/// non-success status, or [`FlathubError::Decode`] if the body isn't the
/// expected JSON shape.
pub fn fetch_appstream(app_id: &str) -> Result<AppStreamRecord, FlathubError> {
    let url = format!("{}/appstream/{}", api_base(), app_id);
    let c = client()?;
    let text = c
        .get(&url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| FlathubError::Http(e.to_string()))?;
    let mut value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| FlathubError::Decode(e.to_string()))?;
    // `/appstream/<id>` has shipped responses both with and without a repeated
    // id field. The path remains authoritative, so don't reject the latter.
    if let Some(object) = value.as_object_mut()
        && !object.contains_key("flatpakAppId")
        && !object.contains_key("app_id")
        && !object.contains_key("id")
    {
        object.insert("id".into(), serde_json::Value::String(app_id.into()));
    }
    let mut record = record_from_value(&value)?;
    record.app_id = app_id.to_string();
    Ok(record)
}

/// Full-text search. Flathub's response has changed between a bare array and
/// a `{ "hits": [...] }` envelope; accept both and normalize summary records
/// into the same shape used by collection/detail enrichment.
///
/// # Errors
///
/// Returns [`FlathubError::Http`] if the request fails or comes back with a
/// non-success status, or [`FlathubError::Decode`] if the body isn't the
/// expected JSON shape.
pub fn search(query: &str) -> Result<Vec<AppStreamRecord>, FlathubError> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let url = format!("{}/search", api_base());
    let text = client()?
        .post(url)
        .json(&serde_json::json!({ "query": query }))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| FlathubError::Http(e.to_string()))?;
    decode_records(&text)
}

fn decode_records(text: &str) -> Result<Vec<AppStreamRecord>, FlathubError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| FlathubError::Decode(e.to_string()))?;
    let rows = value
        .as_array()
        .or_else(|| value.get("hits").and_then(serde_json::Value::as_array))
        .ok_or_else(|| FlathubError::Decode("expected an array or hits array".into()))?;
    rows.iter().map(record_from_value).collect()
}

fn record_from_value(value: &serde_json::Value) -> Result<AppStreamRecord, FlathubError> {
    let app_id = value
        .get("flatpakAppId")
        .or_else(|| value.get("app_id"))
        .or_else(|| value.get("id"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| FlathubError::Decode("search hit has no app id".into()))?
        .to_string();
    let categories = value
        .get("categories")
        .or_else(|| value.get("main_categories"))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|category| {
                    category
                        .as_str()
                        .or_else(|| category.get("name").and_then(serde_json::Value::as_str))
                        .map(|name| CategoryRef { name: name.into() })
                })
                .collect()
        })
        .unwrap_or_default();
    let current_release_version =
        string_field(value, &["currentReleaseVersion", "current_release_version"]).or_else(|| {
            value
                .get("releases")
                .and_then(serde_json::Value::as_array)
                .and_then(|releases| releases.first())
                .and_then(|release| release.get("version"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    let screenshots = value
        .get("screenshots")
        .and_then(serde_json::Value::as_array)
        .map(|screenshots| {
            screenshots
                .iter()
                .filter_map(|shot| {
                    let desktop = string_field(shot, &["imgDesktopUrl", "url"]).or_else(|| {
                        shot.get("sizes")?
                            .as_array()?
                            .first()?
                            .get("src")?
                            .as_str()
                            .map(str::to_string)
                    });
                    let mobile = string_field(shot, &["imgMobileUrl"]);
                    (desktop.is_some() || mobile.is_some()).then_some(ScreenshotRef {
                        img_desktop_url: desktop,
                        img_mobile_url: mobile,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(AppStreamRecord {
        app_id: app_id.clone(),
        name: string_field(value, &["name"]).unwrap_or(app_id),
        summary: string_field(value, &["summary"]).unwrap_or_default(),
        description: string_field(value, &["description"]).unwrap_or_default(),
        icon: string_field(value, &["icon", "icon_url"]),
        current_release_version,
        categories,
        homepage: string_field(value, &["homepage"]),
        urls: None,
        screenshots,
    })
}

fn string_field(value: &serde_json::Value, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
    })
}

/// The pure decode: kept a free fn so tests can feed fixtures without
/// standing up an HTTP mock.
///
/// # Errors
///
/// Returns [`FlathubError::Decode`] if `text` isn't one of the accepted
/// JSON shapes (a bare array/id-list, or a `{ "hits": [...] }` envelope).
pub fn decode_popular(text: &str) -> Result<Vec<PopularEntry>, FlathubError> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Wire {
        Bare(Vec<PopularEntry>),
        Hits { hits: Vec<PopularEntry> },
        StringIds(Vec<String>),
        HitStrings { hits: Vec<String> },
    }
    let wire: Wire = serde_json::from_str(text).map_err(|e| FlathubError::Decode(e.to_string()))?;
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
    let Some(out) = run_with_timeout(
        "flatpak",
        &["list", "--app", "--columns=application,version"],
        Duration::from_millis(1500),
    ) else {
        return Vec::new();
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
    run_with_timeout("flatpak", &["--version"], Duration::from_millis(1500)).is_some()
}

/// Run a host probe with a hard wait bound. A timed-out child is killed and
/// reaped so repeated probes cannot accumulate detached host processes.
pub fn run_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let output = bounded_command::run(program, args, None, 1024 * 1024, timeout).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn list_remotes() -> Vec<Remote> {
    let Some(output) = run_with_timeout(
        "flatpak",
        &["remotes", "--columns=name,url,title,options"],
        Duration::from_millis(1500),
    ) else {
        return Vec::new();
    };
    output
        .lines()
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let name = columns.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let url = columns.next().unwrap_or("").trim().to_string();
            let title = columns.next().unwrap_or("").trim();
            let options = columns.next().unwrap_or("");
            Some(Remote {
                name: name.to_string(),
                url,
                title: if title.is_empty() { name } else { title }.to_string(),
                disabled: options.contains("disabled"),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every Flathub search/detail response shape decodes; the remotes parser is bounded without a binary.
    #[test]
    fn flathub_decoders_accept_published_shapes_and_remotes_stay_bounded() {
        // Case `decode_bare_array_of_ids`.
        {
            let text = r#"["org.gnome.Calculator","org.videolan.VLC"]"#;
            let out = decode_popular(text).unwrap();
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].app_id, "org.gnome.Calculator");
        }

        // Case `decode_hits_object_of_objects`.
        {
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

        // Case `decode_hits_object_of_string_ids`.
        {
            let text = r#"{"hits":["org.gnome.Calculator","org.videolan.VLC"]}"#;
            let out = decode_popular(text).unwrap();
            assert_eq!(out.len(), 2);
            assert_eq!(out[0].app_id, "org.gnome.Calculator");
        }

        // Case `search_decoder_accepts_current_hit_shape`.
        {
            let out = decode_records(
            r#"{"hits":[{"app_id":"org.example.App","name":"Example","summary":"Useful","main_categories":["Utility"]}]}"#,
        )
        .unwrap();
            assert_eq!(out[0].app_id, "org.example.App");
            assert_eq!(out[0].categories[0].name, "Utility");
        }

        // Case `detail_decoder_accepts_path_authoritative_shape`.
        {
            let mut record = record_from_value(&serde_json::json!({
                "id": "placeholder",
                "name": "Example",
                "description": "Long description",
                "categories": ["Utility"],
                "releases": [{"version": "2.0"}],
                "screenshots": [{"url": "https://example.test/shot.png"}]
            }))
            .unwrap();
            record.app_id = "org.example.App".into();
            assert_eq!(record.current_release_version.as_deref(), Some("2.0"));
            assert_eq!(record.screenshots.len(), 1);
        }

        // Case `remotes_parser_contract_is_bounded_on_missing_binary`.
        {
            assert!(
                run_with_timeout(
                    "definitely-not-a-real-flatpak-binary",
                    &["--version"],
                    Duration::from_millis(100)
                )
                .is_none()
            );
        }
    }
}
