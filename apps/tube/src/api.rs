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
//! Real video playback resolves to a **fully-rangeable** proxied stream URL that
//! the shared media engine plays. [`crate::backend::proxy::resolve_rangeable`]
//! probes public Invidious instances for a progressive muxed (video+audio) MP4
//! whose `local=true` proxy serves arbitrary byte ranges over the whole file
//! (raw googlevideo URLs truncate after ~1 minute), falling back to the live
//! backend's direct [`crate::backend::YtBackend::resolve_playback_source`] when
//! no instance answers. The resolved URL comes back as [`Update::StreamResolved`]
//! and is opened into the watch page's `ui::widget::VideoPlayer` engine — which
//! does the actual GStreamer decode + audio out, so no decoder lives here.

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
    /// Resolve a real, directly-playable stream URL for a watch id — off the UI
    /// thread, since it probes public Invidious instances over the network. The
    /// resolved URL comes back as [`Update::StreamResolved`]; the actual decode +
    /// audio out happens in the watch page's media-engine-backed `VideoPlayer`,
    /// not here.
    PlayVideo(String),
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
    /// A real, directly-playable stream URL resolved for a watch id. The UI
    /// opens it into the watch page's `VideoPlayer` engine (real decode + audio).
    StreamResolved {
        video: String,
        url: String,
        has_audio: bool,
        /// Short quality label for the status log (e.g. `360p`).
        quality: String,
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
                thumbnail_url: item
                    .get("videoThumbnails")
                    .and_then(|t| t.as_array())
                    .and_then(|a| a.first())
                    .and_then(|t| t.get("url"))
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
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
                            // First choice is YouTube's dedicated trending browse.
                            // That endpoint (`FEtrending`) has been retired and now
                            // answers HTTP 400 "Request contains an invalid
                            // argument."; there is no no-login browse that returns a
                            // general trending feed. When it fails we compose a real
                            // "popular right now" feed from view-count-sorted,
                            // last-week search across broad topics — genuine YouTube
                            // data, never the offline sample.
                            match feed_trending(&live, offline.as_ref(), &up_tx) {
                                Some((vs, label)) if !vs.is_empty() => {
                                    let _ = up_tx.send(Update::Feed(vs));
                                    let _ = up_tx.send(Update::Status { live: true, label });
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
                        Request::PlayVideo(id) => {
                            // Resolve a real, fully-rangeable stream URL for the
                            // watch page's media engine. Proxy-first (Invidious
                            // `local=true`, which serves whole-file byte ranges),
                            // falling back to the live backend's direct URL.
                            match resolve_stream_url(&live, &id) {
                                Some((url, has_audio, quality)) => {
                                    let _ = up_tx.send(Update::Note(format!(
                                        "stream: {quality} {} [{}]",
                                        if has_audio { "video+audio" } else { "video only" },
                                        short_host(&url)
                                    )));
                                    let _ = up_tx.send(Update::StreamResolved {
                                        video: id,
                                        url,
                                        has_audio,
                                        quality,
                                    });
                                }
                                None => {
                                    let _ = up_tx.send(Update::Note(
                                        "could not resolve a playable stream (no Invidious \
                                         instance served bytes, and no direct URL was available)"
                                            .into(),
                                    ));
                                }
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

/// Short `host…` label for a resolved URL, for the status log.
fn short_host(url: &str) -> String {
    url.split('?')
        .next()
        .and_then(|u| u.split("://").nth(1))
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("stream")
        .to_string()
}

/// Resolve a real, directly-playable stream URL for `id`. Tries the rangeable
/// Invidious proxy first (a progressive muxed video+audio MP4 whose `local=true`
/// endpoint serves whole-file byte ranges — the reliable path for seeking and
/// for streaming past the first minute), then falls back to the live backend's
/// direct playback source (yt-dlp / rustypipe). Returns `(url, has_audio,
/// quality)`, or `None` when nothing is playable.
fn resolve_stream_url(
    live: &Option<Box<dyn YtBackend>>,
    id: &str,
) -> Option<(String, bool, String)> {
    // Primary: yt-dlp resolves a full, non-SABR-gated googlevideo URL (via the
    // `android_vr` client) that streams the WHOLE video+audio without the
    // ~1-minute truncation that gates raw iOS/TV URLs — so the media engine can
    // play the full length and seek. (Proven with `--probe-engine`: full
    // duration, buffered to 100%.)
    if let Some(b) = live.as_ref() {
        if let Ok(src) = b.resolve_playback_source(id) {
            if !src.url.is_empty() {
                return Some((src.url, src.has_audio, src.quality));
            }
        }
    }
    // Fallback: a fully-rangeable Invidious `local=true` proxy. The public
    // instance network has largely collapsed, so this is a last resort for when
    // yt-dlp is unavailable.
    if let Some(r) = crate::backend::proxy::resolve_rangeable(id) {
        return Some((r.url, r.has_audio, r.quality));
    }
    None
}

/// Construct the live rustypipe backend when the `live` feature is on and it
/// initialises. Returns `None` in the default build or on a construction error
/// (the worker then serves the offline sample). Never panics.
#[cfg(feature = "live")]
fn try_live_backend(up_tx: &Sender<Update>) -> Option<Box<dyn YtBackend>> {
    // Primary: yt-dlp, the community-maintained extractor. A subprocess, so it
    // tracks YouTube's changes for us and needs no in-process PoToken/BotGuard.
    match crate::backend::ytdlp::YtDlpBackend::new() {
        Ok(b) => {
            let _ = up_tx.send(Update::Note(format!("live backend ready: {}", b.name())));
            return Some(Box::new(b));
        }
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!(
                "yt-dlp unavailable ({e}); falling back to rustypipe"
            )));
        }
    }
    // Fallback: the in-process rustypipe InnerTube client.
    match crate::backend::live::RustyPipeBackend::new() {
        Ok(b) => {
            let _ = up_tx.send(Update::Note(format!("live backend ready (fallback): {}", b.name())));
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

/// Resolve the Home/Trending catalogue from real data, returning the videos and
/// the honest status-pill label. Tries YouTube's dedicated trending browse
/// first; on failure (the endpoint has been retired) composes a real "popular
/// right now" feed. Returns `None` only when there is no live data at all
/// (offline / default build) so the caller keeps the bundled sample.
fn feed_trending(
    live: &Option<Box<dyn YtBackend>>,
    offline: &dyn YtBackend,
    up_tx: &Sender<Update>,
) -> Option<(Vec<Video>, String)> {
    // Dedicated endpoint first. `fetch` surfaces the real error as a status note
    // (e.g. "live Trending: trending: http status 400"), so the failure of the
    // retired endpoint is visible rather than silently swallowed.
    if let Some((BackendResult::Videos(vs), true)) =
        fetch(live, offline, Query::Trending, false, up_tx)
    {
        if !vs.is_empty() {
            return Some((vs, "live · trending".into()));
        }
    }
    live_popular_feed(live, up_tx).map(|vs| (vs, "live · popular".into()))
}

/// Compose a real "popular right now" home feed: for each broad topic, ask the
/// backend for the most-viewed videos uploaded in the last week, then
/// round-robin interleave for variety and de-duplicate. Every video is genuine
/// YouTube data (view-count-sorted search), not the offline sample. Returns
/// `None` when there is no live backend so the caller keeps the sample.
fn live_popular_feed(
    live: &Option<Box<dyn YtBackend>>,
    up_tx: &Sender<Update>,
) -> Option<Vec<Video>> {
    let backend = live.as_ref()?;
    // Broad, evergreen topics so the union approximates a general popular feed
    // rather than one niche.
    const SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];
    let mut lists: Vec<Vec<Video>> = Vec::new();
    for seed in SEEDS {
        match backend.popular_search(seed) {
            Ok(BackendResult::Videos(vs)) if !vs.is_empty() => lists.push(vs),
            Ok(_) => {}
            Err(e) => {
                let _ = up_tx.send(Update::Note(format!("popular seed {seed:?}: {e}")));
            }
        }
    }
    if lists.is_empty() {
        return None;
    }
    let interleaved = crate::recommend::interleave_newest_first(lists);
    let deduped = dedup_by_id(interleaved);
    let _ = up_tx.send(Update::Note(format!(
        "dedicated trending endpoint retired — composed real popular feed \
         ({} videos, view-sorted across {} topics)",
        deduped.len(),
        SEEDS.len()
    )));
    Some(deduped)
}

/// Drop duplicate videos (same id) while preserving first-seen order — the
/// popular seeds overlap, so the union needs a dedup pass.
fn dedup_by_id(videos: Vec<Video>) -> Vec<Video> {
    let mut seen = std::collections::HashSet::new();
    videos.into_iter().filter(|v| seen.insert(v.id.clone())).collect()
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
