//! Live data sources: Invidious (search + metadata), SponsorBlock
//! (segments) and DeArrow (de-clickbait titles/thumbnails).
//!
//! Everything here runs on a background [`Worker`] thread driven by a
//! blocking `reqwest` client — the `--shots` path never spawns it, so the
//! screenshot is populated purely from `sample.rs`. The app spawns the
//! worker lazily on the first real interaction and folds results in
//! through [`crate::app::TubeApp::pump`].
//!
//! Endpoints (researched, current as of 2026):
//!   * Invidious   `GET {instance}/api/v1/search?q=&type=video`
//!                 `GET {instance}/api/v1/videos/{id}`
//!   * SponsorBlock (privacy hash-prefix mode — only the first 4 hex of
//!                 sha256(videoID) leave the device):
//!                 `GET https://sponsor.ajay.app/api/skipSegments/{prefix}`
//!   * DeArrow     `GET https://sponsor.ajay.app/api/branding/{prefix}`
//!
//! TODO(live): wire the H.264 DASH stream URLs from the Invidious
//! `videos` response into `playback::Playback` / `ui::VideoPlayer`.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::model::{Category, Segment, Video};

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum Request {
    /// Fetch SponsorBlock + DeArrow data for a video being opened.
    Branding(String),
    /// Run an Invidious search.
    Search { instance: String, query: String },
}

/// A result folded back into the model on the UI thread.
#[derive(Clone, Debug)]
pub enum Update {
    Sponsor(String, Vec<Segment>),
    DeArrow {
        video: String,
        title: Option<String>,
        has_thumbnail: bool,
    },
    SearchResults(String, Vec<Video>),
    /// Non-fatal note surfaced in the status log.
    Note(String),
}

/// First 4 lowercase hex chars of sha256(video_id) — the SponsorBlock /
/// DeArrow privacy prefix.
pub fn hash_prefix(video_id: &str) -> String {
    let digest = Sha256::digest(video_id.as_bytes());
    let mut s = String::with_capacity(4);
    for b in digest.iter().take(2) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("tube/0.1 (+qos)")
        .timeout(Duration::from_secs(8))
        .build()
        .expect("reqwest client")
}

/// Fetch SponsorBlock segments using the privacy-preserving hash prefix.
pub fn fetch_sponsor(client: &reqwest::blocking::Client, video_id: &str) -> anyhow::Result<Vec<Segment>> {
    let prefix = hash_prefix(video_id);
    let url = format!("https://sponsor.ajay.app/api/skipSegments/{prefix}");
    let resp: serde_json::Value = client
        .get(&url)
        .query(&[("categories", category_json())])
        .send()?
        .error_for_status()?
        .json()?;

    let mut out = Vec::new();
    if let Some(arr) = resp.as_array() {
        for entry in arr {
            if entry.get("videoID").and_then(|v| v.as_str()) != Some(video_id) {
                continue;
            }
            if let Some(segs) = entry.get("segments").and_then(|s| s.as_array()) {
                for s in segs {
                    let cat = s.get("category").and_then(|c| c.as_str()).unwrap_or("");
                    let times = s.get("segment").and_then(|t| t.as_array());
                    if let (Some(cat), Some(times)) = (Category::from_api_key(cat), times) {
                        let start = times.first().and_then(|x| x.as_f64()).unwrap_or(0.0);
                        let end = times.get(1).and_then(|x| x.as_f64()).unwrap_or(start);
                        let votes = s.get("votes").and_then(|v| v.as_i64()).unwrap_or(0);
                        out.push(Segment { category: cat, start, end, votes });
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

fn category_json() -> String {
    let names: Vec<&str> = Category::ALL.iter().map(|c| c.api_key()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
}

/// Fetch DeArrow branding (top title + whether a community thumbnail
/// exists).
pub fn fetch_dearrow(
    client: &reqwest::blocking::Client,
    video_id: &str,
) -> anyhow::Result<(Option<String>, bool)> {
    let prefix = hash_prefix(video_id);
    let url = format!("https://sponsor.ajay.app/api/branding/{prefix}");
    let resp: serde_json::Value = client.get(&url).send()?.error_for_status()?.json()?;
    let entry = resp.get(video_id);
    let title = entry
        .and_then(|e| e.get("titles"))
        .and_then(|t| t.as_array())
        .and_then(|a| a.first())
        .and_then(|t| t.get("title"))
        .and_then(|t| t.as_str())
        .map(|s| s.to_string());
    let has_thumb = entry
        .and_then(|e| e.get("thumbnails"))
        .and_then(|t| t.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    Ok((title, has_thumb))
}

/// Minimal Invidious search → [`Video`] mapping. Best-effort; missing
/// fields fall back to sensible defaults.
pub fn search(
    client: &reqwest::blocking::Client,
    instance: &str,
    query: &str,
) -> anyhow::Result<Vec<Video>> {
    let url = format!("{}/api/v1/search", instance.trim_end_matches('/'));
    let resp: serde_json::Value = client
        .get(&url)
        .query(&[("q", query), ("type", "video")])
        .send()?
        .error_for_status()?
        .json()?;

    let mut out = Vec::new();
    if let Some(arr) = resp.as_array() {
        for item in arr.iter().take(40) {
            if item.get("type").and_then(|t| t.as_str()) != Some("video") {
                continue;
            }
            let id = item.get("videoId").and_then(|v| v.as_str()).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let title = item.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
            let author = item.get("author").and_then(|v| v.as_str()).unwrap_or("Unknown");
            let author_id = item.get("authorId").and_then(|v| v.as_str()).unwrap_or(author);
            let views = item.get("viewCount").and_then(|v| v.as_u64()).unwrap_or(0);
            let duration = item.get("lengthSeconds").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let published = item
                .get("publishedText")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            out.push(Video {
                id: id.to_string(),
                title: title.to_string(),
                dearrow_title: None,
                channel_id: author_id.to_string(),
                channel_name: author.to_string(),
                channel_seed: author_id.to_string(),
                views,
                published,
                duration,
                description: item
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                likes: 0,
                seed: id.to_string(),
                category: "Search".to_string(),
                dearrow_thumb: false,
                sponsor: vec![],
                comments: vec![],
                live: item.get("liveNow").and_then(|v| v.as_bool()).unwrap_or(false),
            });
        }
    }
    Ok(out)
}

/// The background worker: owns the blocking HTTP client and turns
/// [`Request`]s into [`Update`]s.
pub struct Worker {
    tx: Sender<Request>,
    rx: Receiver<Update>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<Request>();
        let (up_tx, up_rx) = std::sync::mpsc::channel::<Update>();
        std::thread::Builder::new()
            .name("tube-fetch".into())
            .spawn(move || {
                let client = client();
                while let Ok(req) = req_rx.recv() {
                    match req {
                        Request::Branding(id) => {
                            match fetch_sponsor(&client, &id) {
                                Ok(segs) => {
                                    let _ = up_tx.send(Update::Sponsor(id.clone(), segs));
                                }
                                Err(e) => {
                                    let _ = up_tx.send(Update::Note(format!("SponsorBlock: {e}")));
                                }
                            }
                            match fetch_dearrow(&client, &id) {
                                Ok((title, has)) => {
                                    let _ = up_tx.send(Update::DeArrow {
                                        video: id,
                                        title,
                                        has_thumbnail: has,
                                    });
                                }
                                Err(e) => {
                                    let _ = up_tx.send(Update::Note(format!("DeArrow: {e}")));
                                }
                            }
                        }
                        Request::Search { instance, query } => match search(&client, &instance, &query) {
                            Ok(vs) => {
                                let _ = up_tx.send(Update::SearchResults(query, vs));
                            }
                            Err(e) => {
                                let _ = up_tx.send(Update::Note(format!("Search: {e}")));
                            }
                        },
                    }
                }
            })
            .expect("spawn tube-fetch");
        Self { tx: req_tx, rx: up_rx }
    }

    pub fn request(&self, req: Request) {
        let _ = self.tx.send(req);
    }

    /// Non-blocking drain of any results ready this frame.
    pub fn poll(&self) -> Vec<Update> {
        self.rx.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_prefix_is_4_hex() {
        let p = hash_prefix("dQw4w9WgXcQ");
        assert_eq!(p.len(), 4);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn category_json_lists_all() {
        let j = category_json();
        assert!(j.contains("sponsor"));
        assert!(j.contains("music_offtopic"));
    }
}
