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

use crate::backend::{BackendResult, Query, YtBackend};
use crate::model::{Category, Channel, Segment, Video};

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum Request {
    /// Fetch SponsorBlock + DeArrow data for a video being opened.
    Branding(String),
    /// Run a search through the data-source backend.
    Search { instance: String, query: String },
    /// Fetch the home / trending catalogue. Only a *live* backend answers this
    /// — the offline "feed" is just the sample already on screen, so a failed
    /// or absent live backend leaves the bundled catalogue untouched.
    Feed,
    /// Fetch full metadata + real "up next" recommendations for a video the
    /// user just opened.
    VideoMeta(String),
    /// Fetch a channel page + its uploads.
    Channel(String),
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
    /// Live home/trending catalogue — replaces the on-screen set.
    Feed(Vec<Video>),
    /// Live video detail + its real recommendations.
    VideoMeta {
        video: Box<Video>,
        related: Vec<Video>,
    },
    /// Live channel page + uploads.
    ChannelPage {
        channel: Box<Channel>,
        uploads: Vec<Video>,
    },
    /// Which data source is currently answering (drives the top-bar pill).
    Status {
        live: bool,
        label: String,
    },
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
                // The data-source seam. The offline sample is always available
                // as a fallback; the live rustypipe backend is present only
                // under `--features live` *and* if it constructed. SponsorBlock /
                // DeArrow keep their own direct `sponsor.ajay.app` path (they are
                // orthogonal to the InnerTube source).
                let offline = crate::backend::default_backend();
                let live = try_live_backend(&up_tx);
                // Announce the starting posture honestly: even with a live
                // backend constructed we don't claim "live" until a real fetch
                // returns, because construction doesn't touch the network.
                let _ = up_tx.send(Update::Status {
                    live: false,
                    label: if live.is_some() {
                        "connecting…".into()
                    } else {
                        "offline — sample data".into()
                    },
                });
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
                        Request::Search { query, .. } => {
                            // Live first, then fall back to the offline sample so
                            // search always returns something usable.
                            if let Some((BackendResult::Videos(vs), used_live)) =
                                fetch(&live, offline.as_ref(), Query::Search(query.clone()), true, &up_tx)
                            {
                                send_status(&up_tx, used_live);
                                let _ = up_tx.send(Update::SearchResults(query, vs));
                            }
                        }
                        Request::Feed => {
                            // Live only: an offline feed is the sample already on
                            // screen, so a missing/failed live backend just marks
                            // the source offline and leaves the catalogue as-is.
                            //
                            // Primary source is the real trending browse. YouTube
                            // currently rejects rustypipe 0.11's `FEtrending`
                            // params ("invalid argument"), so when that fails we
                            // compose a live home from a handful of broad topic
                            // searches instead — still real data, no sample.
                            let feed = match fetch(&live, offline.as_ref(), Query::Trending, false, &up_tx) {
                                Some((BackendResult::Videos(vs), true)) if !vs.is_empty() => Some(vs),
                                _ => live_home_from_search(&live, &up_tx),
                            };
                            match feed {
                                Some(vs) if !vs.is_empty() => {
                                    let _ = up_tx.send(Update::Feed(vs));
                                    send_status(&up_tx, true);
                                }
                                _ => send_status(&up_tx, false),
                            }
                        }
                        Request::VideoMeta(id) => {
                            // Real "up next" comes off the detail response; fetch
                            // it alongside the metadata (live only — the sample
                            // already carries full detail for its own ids).
                            let related =
                                match fetch(&live, offline.as_ref(), Query::Related(id.clone()), false, &up_tx) {
                                    Some((BackendResult::Videos(r), _)) => r,
                                    _ => Vec::new(),
                                };
                            if let Some((BackendResult::Video(v), used_live)) =
                                fetch(&live, offline.as_ref(), Query::Video(id.clone()), false, &up_tx)
                            {
                                send_status(&up_tx, used_live);
                                let _ = up_tx.send(Update::VideoMeta { video: v, related });
                            }
                        }
                        Request::Channel(id) => {
                            if let Some((BackendResult::Channel { channel, uploads }, used_live)) =
                                fetch(&live, offline.as_ref(), Query::Channel(id), false, &up_tx)
                            {
                                send_status(&up_tx, used_live);
                                let _ = up_tx.send(Update::ChannelPage { channel, uploads });
                            }
                        }
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

/// Construct the live rustypipe backend when the `live` feature is on and it
/// initialises. Returns `None` in the default build or on a construction error
/// (the worker then serves the offline sample). Never panics.
#[cfg(feature = "live")]
fn try_live_backend(up_tx: &Sender<Update>) -> Option<Box<dyn YtBackend>> {
    match crate::backend::live::RustyPipeBackend::new() {
        Ok(b) => {
            let _ = up_tx.send(Update::Note(format!("live backend ready: {}", b.name())));
            Some(Box::new(b))
        }
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!(
                "live backend unavailable ({e}); using offline sample"
            )));
            None
        }
    }
}

#[cfg(not(feature = "live"))]
fn try_live_backend(_up_tx: &Sender<Update>) -> Option<Box<dyn YtBackend>> {
    None
}

/// Run `query` against the live backend first (when present); on a live error,
/// optionally fall back to the offline sample. Returns the result together with
/// whether the *live* backend produced it — that boolean drives the on-screen
/// "Live" vs "Sample" status. Live errors are surfaced as a status note.
fn fetch(
    live: &Option<Box<dyn YtBackend>>,
    offline: &dyn YtBackend,
    query: Query,
    fallback: bool,
    up_tx: &Sender<Update>,
) -> Option<(BackendResult, bool)> {
    if let Some(b) = live {
        match b.run(query.clone()) {
            Ok(r) => return Some((r, true)),
            Err(e) => {
                let _ = up_tx.send(Update::Note(format!("live {query:?}: {e}")));
            }
        }
    }
    if fallback {
        if let Ok(r) = offline.run(query) {
            return Some((r, false));
        }
    }
    None
}

/// Compose a live home feed from a few broad topic searches, interleaved for
/// variety. Used as a fallback for the Home/Trending catalogue when YouTube
/// rejects rustypipe's dedicated trending browse. Returns `None` when there is
/// no live backend (offline / default build) so the caller keeps the sample.
fn live_home_from_search(
    live: &Option<Box<dyn YtBackend>>,
    up_tx: &Sender<Update>,
) -> Option<Vec<Video>> {
    let backend = live.as_ref()?;
    const SEEDS: [&str; 3] = ["technology", "science", "music"];
    let mut lists: Vec<Vec<Video>> = Vec::new();
    for seed in SEEDS {
        match backend.search(seed) {
            Ok(BackendResult::Videos(vs)) if !vs.is_empty() => lists.push(vs),
            Ok(_) => {}
            Err(e) => {
                let _ = up_tx.send(Update::Note(format!("home seed {seed:?}: {e}")));
            }
        }
    }
    if lists.is_empty() {
        return None;
    }
    let _ = up_tx.send(Update::Note(
        "trending unavailable — composed live home from topic searches".into(),
    ));
    Some(crate::recommend::interleave_newest_first(lists))
}

/// Broadcast which data source just answered so the UI pill reflects reality.
fn send_status(up_tx: &Sender<Update>, live: bool) {
    let label = if live {
        "live (rustypipe)"
    } else {
        "offline — sample data"
    };
    let _ = up_tx.send(Update::Status { live, label: label.into() });
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
