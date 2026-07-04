//! `UninstalledFlatpakSource` — the top ~500 popular Flathub apps
//! the store surfaces as one-click-installable rows.
//!
//! Flathub's public API exposes a stable-ish JSON endpoint at
//! `https://flathub.org/api/v2/popular/last-week?limit=500`. Each
//! row carries an `app_id`, `name`, `summary`, and a category list.
//!
//! Refresh does one HTTP GET with a bounded timeout. On failure we
//! keep the previous items (aggregator returns the error) so the
//! palette still renders whatever was cached.

use std::time::Instant;

use serde::Deserialize;

use super::{SearchAction, SearchItem, SearchSource, SourceError, SourceKind};

const FLATHUB_POPULAR_URL: &str =
    "https://flathub.org/api/v2/popular/last-week?limit=500";

#[derive(Debug, Deserialize, Clone)]
struct FlathubRow {
    #[serde(rename = "app_id")]
    app_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    categories: Vec<String>,
}

pub struct UninstalledFlatpakSource {
    fetcher: FetchBox,
    items: Vec<SearchItem>,
    last_refresh: Option<Instant>,
}

type FetchBox = Box<
    dyn Fn() -> Result<Vec<serde_json::Value>, SourceError> + Send + Sync,
>;

impl UninstalledFlatpakSource {
    pub fn new() -> Self {
        Self {
            fetcher: Box::new(fetch_via_reqwest_blocking),
            items: Vec::new(),
            last_refresh: None,
        }
    }

    pub fn with_fetcher<F>(mut self, fetcher: F) -> Self
    where
        F: Fn() -> Result<Vec<serde_json::Value>, SourceError>
            + Send
            + Sync
            + 'static,
    {
        self.fetcher = Box::new(fetcher);
        self
    }
}

impl Default for UninstalledFlatpakSource {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchSource for UninstalledFlatpakSource {
    fn kind(&self) -> SourceKind {
        SourceKind::UninstalledFlatpak
    }

    fn items(&self) -> &[SearchItem] {
        &self.items
    }

    fn refresh(&mut self) -> Result<(), SourceError> {
        let values = (self.fetcher)()?;
        let mut items = Vec::with_capacity(values.len());
        for v in values {
            let Ok(row) = serde_json::from_value::<FlathubRow>(v) else {
                continue;
            };
            let name = if row.name.is_empty() { row.app_id.clone() } else { row.name };
            items.push(SearchItem {
                id: row.app_id.clone(),
                name,
                description: row.summary,
                icon: None,
                source: SourceKind::UninstalledFlatpak,
                categories: {
                    let mut c = row.categories;
                    c.push("Flatpak".into());
                    c
                },
                action: SearchAction::InstallFlatpak {
                    flatpak_id: row.app_id,
                },
            });
        }
        self.items = items;
        self.last_refresh = Some(Instant::now());
        Ok(())
    }

    fn last_refreshed_at(&self) -> Option<Instant> {
        self.last_refresh
    }
}

fn fetch_via_reqwest_blocking() -> Result<Vec<serde_json::Value>, SourceError> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .user_agent("qos-shell-search/0.1")
        .build()
        .map_err(|e| SourceError::Fetch(e.to_string()))?;
    let resp = client
        .get(FLATHUB_POPULAR_URL)
        .send()
        .map_err(|e| SourceError::Fetch(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(SourceError::Fetch(format!("HTTP {}", resp.status())));
    }
    let arr: Vec<serde_json::Value> =
        resp.json().map_err(|e| SourceError::Parse(e.to_string()))?;
    Ok(arr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flathub_rows() {
        let mut src = UninstalledFlatpakSource::new().with_fetcher(|| {
            Ok(vec![
                serde_json::json!({
                    "app_id": "org.gnome.Weather",
                    "name": "Weather",
                    "summary": "See what the weather looks like",
                    "categories": ["Utility"],
                }),
                serde_json::json!({
                    "app_id": "com.spotify.Client",
                    "name": "Spotify",
                    "summary": "Music streaming",
                    "categories": ["Audio"],
                }),
            ])
        });
        src.refresh().unwrap();
        assert_eq!(src.items().len(), 2);
        let names: Vec<&str> = src.items().iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Weather"));
        assert!(names.contains(&"Spotify"));
    }

    #[test]
    fn fetch_error_bubbles_up() {
        let mut src = UninstalledFlatpakSource::new()
            .with_fetcher(|| Err(SourceError::Fetch("network is down".into())));
        let err = src.refresh().unwrap_err();
        assert!(matches!(err, SourceError::Fetch(_)));
    }
}
