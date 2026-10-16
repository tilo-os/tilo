//! Flathub JSON API v2 client — the "legacy apps" feed.
//!
//! We only touch the endpoints the store actually renders:
//!
//!   * `GET /collection/popular?page=1&per_page=200` — popular apps.
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

/// A configured Flatpak remote, as reported by the machine-readable CLI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub title: String,
    pub disabled: bool,
    /// System remotes are supplied by the image and cannot be deleted as this user.
    pub user_managed: bool,
    /// Last configured-source read failure; independent sources remain usable.
    pub catalogue_error: Option<String>,
}

impl Remote {
    pub fn resource_id(&self) -> String {
        if self.user_managed {
            format!("remote:{}", self.name)
        } else {
            format!("remote:system:{}", self.name)
        }
    }
}

/// A metadata-only refresh must not claim a failed catalogue recovered.
/// Changed identity, URL or disabled state needs a new source observation.
pub fn refreshed_remotes(rows: &str, previous: &[Remote]) -> Vec<Remote> {
    let mut remotes = list_remotes(rows);
    for remote in &mut remotes {
        if !remote.disabled
            && let Some(old) = previous.iter().find(|old| {
                old.resource_id() == remote.resource_id() && old.url == remote.url && !old.disabled
            })
        {
            remote.catalogue_error.clone_from(&old.catalogue_error);
        }
    }
    remotes
}

/// Explain a device repository's known permission refusal in human terms.
pub fn remote_error(remote: &Remote, error: &str) -> String {
    tracing::warn!(repository = %remote.name, %error, "could not read Flatpak repository");
    if !remote.user_managed && error.contains("GenerateOciSummary") {
        ui::i18n::I18nManager::format(
            "store-device-repository-refresh",
            &[("name", ui::i18n::ArgValue::Text(remote.title.clone()))],
        )
    } else if error.contains("command timed out after ") {
        ui::i18n::I18nManager::format(
            "store-repository-timeout",
            &[("name", ui::i18n::ArgValue::Text(remote.title.clone()))],
        )
    } else {
        ui::i18n::I18nManager::format(
            "store-repository-read-error",
            &[
                ("name", ui::i18n::ArgValue::Text(remote.title.clone())),
                ("error", ui::i18n::ArgValue::Text(error.to_string())),
            ],
        )
    }
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
pub fn fetch_popular(max_apps: usize) -> Result<Vec<AppStreamRecord>, FlathubError> {
    if max_apps == 0 {
        return Ok(Vec::new());
    }
    let text = client()?
        .get(format!(
            "{}/collection/popular?page=1&per_page={}",
            api_base(),
            max_apps.min(200)
        ))
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| FlathubError::Http(e.to_string()))?;
    let mut records = decode_records(&text)?;
    records.truncate(max_apps);
    Ok(records)
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
    rows.iter()
        .filter(|row| {
            row.get("arches")
                .and_then(serde_json::Value::as_array)
                .is_none_or(|arches| {
                    arches
                        .iter()
                        .any(|arch| arch.as_str() == Some(std::env::consts::ARCH))
                })
        })
        .map(record_from_value)
        .collect()
}

fn record_from_value(value: &serde_json::Value) -> Result<AppStreamRecord, FlathubError> {
    let app_id = value
        .get("app_id")
        .or_else(|| value.get("flatpakAppId"))
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

/// Decode the host service's machine-readable installed-app rows.
pub fn installed_flatpaks(output: &str) -> Vec<(String, String)> {
    let mut installed = std::collections::BTreeMap::<String, (String, bool)>::new();
    for line in output.lines() {
        let columns: Vec<_> = line.split('\t').collect();
        let Some(id) = columns
            .first()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let version = columns.get(1).copied().unwrap_or("").trim().to_string();
        let user = columns.get(3).is_some_and(|scope| scope.trim() == "user");
        let entry = installed
            .entry(id.to_string())
            .or_insert_with(|| (version.clone(), user));
        if user {
            *entry = (version, true);
        }
    }
    installed
        .into_iter()
        .map(|(id, (version, _))| (id, version))
        .collect()
}

/// Preserve the repository and scope, preferring this account's installation
/// when the same app is also supplied by the device.
pub fn installed_origins(output: &str) -> std::collections::HashMap<String, (String, bool)> {
    let mut installed = std::collections::HashMap::<String, (String, bool)>::new();
    for line in output.lines() {
        let columns: Vec<_> = line.split('\t').collect();
        let Some(id) = columns
            .first()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        let Some(origin) = columns.get(2).map(|origin| origin.trim()) else {
            continue;
        };
        let user = columns.get(3).is_some_and(|scope| scope.trim() == "user");
        let entry = installed
            .entry(id.to_string())
            .or_insert_with(|| (origin.to_string(), user));
        if user {
            *entry = (origin.to_string(), true);
        }
    }
    installed
}

/// Decode remote rows read outside the app sandbox by app-host.
pub fn list_remotes(output: &str) -> Vec<Remote> {
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
                user_managed: !options.split(',').any(|option| option.trim() == "system"),
                disabled: options.split(',').any(|option| option.trim() == "disabled"),
                catalogue_error: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_refresh_preserves_only_the_same_enabled_source_failure() {
        let rows = "fedora\thttps://registry.example/repo\tFedora\tsystem\n";
        let mut old = list_remotes(rows);
        old[0].catalogue_error = Some("Unable to load summary".into());
        assert_eq!(
            refreshed_remotes(rows, &old)[0].catalogue_error,
            old[0].catalogue_error
        );
        for changed in [
            "fedora\thttps://other.example/repo\tFedora\tsystem\n",
            "fedora\thttps://registry.example/repo\tFedora\tuser\n",
            "fedora\thttps://registry.example/repo\tFedora\tsystem,disabled\n",
        ] {
            assert!(
                refreshed_remotes(changed, &old)[0]
                    .catalogue_error
                    .is_none()
            );
        }
        assert!(refreshed_remotes("", &old).is_empty());
    }

    #[test]
    fn repository_timeout_explains_retry_without_rpc_internals() {
        crate::resource::STORE_CATALOG.ensure();
        let remote = Remote {
            name: "fedora".into(),
            url: "https://example.invalid/fedora".into(),
            title: "Fedora Flatpaks".into(),
            disabled: false,
            user_managed: false,
            catalogue_error: None,
        };
        let message = remote_error(
            &remote,
            "Failed: remote exception: remote exception: command timed out after 8000 ms",
        );
        assert_eq!(
            message,
            ui::i18n::I18nManager::format(
                "store-repository-timeout",
                &[("name", ui::i18n::ArgValue::Text("Fedora Flatpaks".into()))],
            )
        );
        assert!(message.contains("Fedora Flatpaks"));
        assert!(!message.contains("remote exception"));
        assert!(!message.contains("8000"));
    }

    #[test]
    fn installed_account_copy_wins_over_device_copy_in_either_row_order() {
        for rows in [
            "org.example.Editor\t1.0\tdevice-tools\tsystem\norg.example.Editor\t2.0\ttools\tuser\n",
            "org.example.Editor\t2.0\ttools\tuser\norg.example.Editor\t1.0\tdevice-tools\tsystem\n",
        ] {
            assert_eq!(
                installed_flatpaks(rows),
                vec![("org.example.Editor".into(), "2.0".into())]
            );
            assert_eq!(
                installed_origins(rows).get("org.example.Editor"),
                Some(&("tools".into(), true))
            );
        }
    }

    #[test]
    fn popular_request_uses_current_collection_and_canonical_app_ids() {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("FLATHUB_API_BASE", format!("http://{address}/api/v2"));
        env.set("TILO_STORE_HTTP_TIMEOUT", "2");
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "no collection request received"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("collection request failed: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut buffer = [0; 4096];
            let size = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..size]).into_owned();
            let body = r#"{"hits":[{"id":"org_gnome_Calculator","app_id":"org.gnome.Calculator","name":"Calculator","summary":"Solve equations","main_categories":["Utility"]}]}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            request
        });
        let rows = fetch_popular(200).unwrap();
        let request = server.join().unwrap();
        assert!(
            request.starts_with("GET /api/v2/collection/popular?page=1&per_page=200 HTTP/1.1"),
            "{request}"
        );
        assert_eq!(rows[0].app_id, "org.gnome.Calculator");
        let item = crate::sources::flathub_record_to_item(&rows[0], &[]);
        assert_eq!(item.name, "Calculator");
        assert_eq!(item.tagline, "Solve equations");
        assert_eq!(item.categories, vec!["Utility"]);
    }

    /// Every Flathub search/detail response shape decodes; the remotes parser is bounded without a binary.
    #[test]
    fn flathub_decoders_accept_published_shapes_and_remotes_stay_bounded() {
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

        // Collection IDs are search-index keys; only app_id is a Flatpak ref.
        let current_collection = r#"{"hits":[{"id":"org_gnome_Calculator","app_id":"org.gnome.Calculator","name":"Calculator","summary":"Solve equations","icon":"https://dl.flathub.org/icon.png","main_categories":["Utility"]}],"page":1,"totalPages":1}"#;
        let records = decode_records(current_collection).unwrap();
        assert_eq!(records[0].app_id, "org.gnome.Calculator");
        assert_eq!(records[0].summary, "Solve equations");
        assert_eq!(records[0].categories[0].name, "Utility");

        let architecture_rows = serde_json::json!({"hits": [
            {"app_id": "org.example.Native", "arches": [std::env::consts::ARCH]},
            {"app_id": "org.example.Foreign", "arches": ["different-device"]}
        ]});
        let native = decode_records(&architecture_rows.to_string()).unwrap();
        assert_eq!(native.len(), 1);
        assert_eq!(native[0].app_id, "org.example.Native");

        // Host rows include system and user installations, and disabled remotes.
        let rows = list_remotes(
            "flathub\thttps://dl.flathub.org/repo/\tFlathub\tsystem\nfedora\toci+https://registry.fedoraproject.org\tFedora\tsystem,disabled\n",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].title, "Flathub");
        assert!(!rows[0].disabled);
        assert!(rows[1].disabled);
        assert_eq!(
            installed_flatpaks("org.gnome.Calculator\t48.0\n"),
            vec![("org.gnome.Calculator".into(), "48.0".into())]
        );
    }
}
