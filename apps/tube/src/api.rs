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
//!   * SponsorBlock `GET https://sponsor.ajay.app/api/skipSegments?videoID=<id>&categories=[…]`
//!   * DeArrow     `GET https://sponsor.ajay.app/api/branding?videoID=<id>`
//!   * Dislikes    `GET https://returnyoutubedislikeapi.com/votes?videoId=<id>`
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

use crate::backend::{BackendResult, Query, YtBackend};
use crate::model::{Category, Channel, Comment, Playlist, Segment, Video};

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum Request {
    /// Fetch SponsorBlock + DeArrow data for a video being opened.
    Branding(String),
    /// Run a search through the data-source backend.
    Search { query: String },
    /// Fetch the home / trending catalogue. Only a *live* backend answers this
    /// — the offline "feed" is just the sample already on screen, so a failed
    /// or absent live backend leaves the bundled catalogue untouched.
    Feed,
    /// Fetch full metadata + real "up next" recommendations for a video the
    /// user just opened.
    VideoMeta(String),
    /// Fetch a channel page + its uploads.
    Channel(String),
    /// Fetch a playlist page (metadata + ordered video list).
    PlaylistPage(String),
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
        /// Top-voted crowdsourced title, when one outranks the original.
        title: Option<String>,
        /// Community thumbnail URL (a frame rendered at the top-voted timestamp),
        /// when one exists.
        thumbnail_url: Option<String>,
    },
    /// Crowdsourced dislike count for a video (Return YouTube Dislike).
    Dislikes {
        video: String,
        count: u64,
    },
    /// Live search results: the query, the ordered videos, and the playlists +
    /// creator rows that share the query (so the type filter has all three).
    SearchResults {
        query: String,
        videos: Vec<Video>,
        playlists: Vec<Playlist>,
        channels: Vec<Channel>,
    },
    /// A resolved playlist page + its ordered videos.
    PlaylistPage {
        playlist: Box<Playlist>,
        videos: Vec<Video>,
    },
    /// Live home/trending catalogue — replaces the on-screen set.
    Feed(Vec<Video>),
    /// Live video detail + its real recommendations.
    VideoMeta {
        video: Box<Video>,
        related: Vec<Video>,
    },
    /// Real top comments for a video (fetched via rustypipe), folded onto the
    /// watch page.
    Comments {
        video: String,
        comments: Vec<Comment>,
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

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder()
        .user_agent("tube/0.1 (+qos)")
        .timeout(Duration::from_secs(8))
        .build()
        .expect("reqwest client")
}

/// Fetch SponsorBlock segments for a video via the direct
/// `GET /api/skipSegments?videoID=<id>&categories=[…]` endpoint. A 404 (the
/// API's "no segments for this video" response) is treated as an empty list
/// rather than an error.
pub fn fetch_sponsor(client: &reqwest::blocking::Client, video_id: &str) -> anyhow::Result<Vec<Segment>> {
    let cats = category_json();
    let resp = client
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&[("videoID", video_id), ("categories", cats.as_str())])
        .send()?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }
    let val: serde_json::Value = resp.error_for_status()?.json()?;
    Ok(parse_skip_segments(&val))
}

/// Parse a SponsorBlock `/api/skipSegments?videoID=<id>` response (a flat JSON
/// array of segment objects) into sorted [`Segment`]s. Pure (no I/O) so it is
/// unit-tested directly. Unknown categories and malformed rows are skipped.
pub fn parse_skip_segments(resp: &serde_json::Value) -> Vec<Segment> {
    let mut out = Vec::new();
    if let Some(arr) = resp.as_array() {
        for s in arr {
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
    out.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn category_json() -> String {
    let names: Vec<&str> = Category::ALL.iter().map(|c| c.api_key()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
}

/// Fetch DeArrow branding for a video: the top-voted crowdsourced title (when it
/// outranks the original) and a community thumbnail URL (a frame rendered at the
/// top-voted timestamp), via `GET /api/branding?videoID=<id>`.
pub fn fetch_dearrow(
    client: &reqwest::blocking::Client,
    video_id: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let resp: serde_json::Value = client
        .get("https://sponsor.ajay.app/api/branding")
        .query(&[("videoID", video_id)])
        .send()?
        .error_for_status()?
        .json()?;
    Ok(parse_dearrow(&resp, video_id))
}

/// Parse a DeArrow `/api/branding?videoID=<id>` response into `(title,
/// thumbnail_url)`. Picks the highest-scored non-original entry (a locked entry
/// wins over vote count, matching DeArrow's own precedence), and skips entries
/// the community has voted down. Pure (no I/O) so it is unit-tested directly.
pub fn parse_dearrow(
    resp: &serde_json::Value,
    video_id: &str,
) -> (Option<String>, Option<String>) {
    // Score an entry: locked entries win outright; otherwise by vote count.
    // Downvoted-into-removal entries (votes < 0 and not locked) are dropped.
    let score = |e: &serde_json::Value| -> Option<i64> {
        let original = e.get("original").and_then(|o| o.as_bool()).unwrap_or(false);
        if original {
            return None;
        }
        let votes = e.get("votes").and_then(|v| v.as_i64()).unwrap_or(0);
        let locked = e.get("locked").and_then(|l| l.as_bool()).unwrap_or(false);
        if votes < 0 && !locked {
            return None;
        }
        Some(if locked { votes + 1_000_000 } else { votes })
    };

    let title = resp
        .get("titles")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|e| Some((score(e)?, e)))
                .max_by_key(|(s, _)| *s)
                .and_then(|(_, e)| e.get("title").and_then(|s| s.as_str()))
                .map(|s| s.to_string())
        });

    let thumbnail_url = resp
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let s = score(e)?;
                    let ts = e.get("timestamp").and_then(|v| v.as_f64())?;
                    Some((s, ts))
                })
                .max_by_key(|(s, _)| *s)
                .map(|(_, ts)| {
                    format!(
                        "https://dearrow-thumb.ajay.app/api/v1/getThumbnail?videoID={video_id}&time={ts}"
                    )
                })
        });

    (title, thumbnail_url)
}

/// Fetch the crowdsourced dislike count for a video via Return YouTube Dislike
/// (`GET /votes?videoId=<id>`).
pub fn fetch_dislikes(
    client: &reqwest::blocking::Client,
    video_id: &str,
) -> anyhow::Result<u64> {
    let resp: serde_json::Value = client
        .get("https://returnyoutubedislikeapi.com/votes")
        .query(&[("videoId", video_id)])
        .send()?
        .error_for_status()?
        .json()?;
    parse_dislikes(&resp).ok_or_else(|| anyhow::anyhow!("no dislikes field in RYD response"))
}

/// Parse a Return YouTube Dislike `/votes` response's dislike count. Pure (no
/// I/O) so it is unit-tested directly.
pub fn parse_dislikes(resp: &serde_json::Value) -> Option<u64> {
    resp.get("dislikes").and_then(|v| v.as_u64())
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
                channel_avatar_url: item
                    .get("authorThumbnails")
                    .and_then(|t| t.as_array())
                    .and_then(|a| a.last())
                    .and_then(|t| t.get("url"))
                    .and_then(|u| u.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
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
                // One lazily-constructed rustypipe backend serves everything the
                // primary (yt-dlp) source can't: top comments, the creator avatars
                // + playlists + creator rows that enrich a search (yt-dlp list
                // items carry none of these), avatar-bearing popular feed videos,
                // and playlist pages. Constructed on first need so a session that
                // never searches/opens a video pays nothing. Outer `Option` =
                // "attempted yet?"; `#[cfg(live)]` only — the offline build has no
                // rustypipe and simply skips every enrichment below.
                #[cfg(feature = "live")]
                let mut rp: Option<Option<crate::backend::live::RustyPipeBackend>> = None;
                // Announce the starting posture honestly: even with a live
                // backend constructed we don't claim "live" until a real fetch
                // returns, because construction doesn't touch the network.
                let _ = up_tx.send(Update::Status {
                    live: false,
                    label: if live.is_some() {
                        "connecting…".into()
                    } else {
                        "offline".into()
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
                                Ok((title, thumbnail_url)) => {
                                    let _ = up_tx.send(Update::DeArrow {
                                        video: id.clone(),
                                        title,
                                        thumbnail_url,
                                    });
                                }
                                Err(e) => {
                                    let _ = up_tx.send(Update::Note(format!("DeArrow: {e}")));
                                }
                            }
                            match fetch_dislikes(&client, &id) {
                                Ok(count) => {
                                    let _ = up_tx.send(Update::Dislikes { video: id, count });
                                }
                                Err(e) => {
                                    let _ = up_tx.send(Update::Note(format!("Dislikes: {e}")));
                                }
                            }
                        }
                        Request::Search { query } => {
                            // Live only — NEVER fall back to the bundled sample:
                            // presenting fabricated videos as real search results
                            // is exactly what the "no fake content" rule forbids.
                            // Always emit a (possibly empty) result so the UI can
                            // drop its loading skeleton and show either real hits
                            // or an honest "no results".
                            let mut videos = match fetch(
                                &live,
                                offline.as_ref(),
                                Query::Search(query.clone()),
                                false,
                                &up_tx,
                            ) {
                                Some((BackendResult::Videos(vs), used_live)) => {
                                    send_status(&up_tx, used_live);
                                    vs
                                }
                                _ => Vec::new(),
                            };
                            let mut playlists: Vec<Playlist> = Vec::new();
                            let mut channels: Vec<Channel> = Vec::new();
                            // Enrich with rustypipe: real creator avatars for the
                            // yt-dlp videos (which have none), plus the playlists
                            // and creator rows for the same query — all from one
                            // real InnerTube search call.
                            #[cfg(feature = "live")]
                            {
                                let rp_ref = rp.get_or_insert_with(|| try_enricher(&up_tx));
                                if let Some(b) = rp_ref.as_ref() {
                                    match b.search_items(&query) {
                                        Ok(items) => {
                                            enrich_video_avatars(&mut videos, &items);
                                            // If the primary returned nothing, fall
                                            // back to rustypipe's own videos (already
                                            // avatar-bearing) rather than an empty page.
                                            if videos.is_empty() {
                                                videos = items.videos;
                                            }
                                            playlists = items.playlists;
                                            channels = items.channels;
                                        }
                                        Err(e) => {
                                            let _ = up_tx
                                                .send(Update::Note(format!("search enrich: {e}")));
                                        }
                                    }
                                }
                            }
                            let _ = up_tx.send(Update::SearchResults {
                                query,
                                videos,
                                playlists,
                                channels,
                            });
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
                            // Prefer rustypipe for the composed popular feed: its
                            // items carry creator avatars inline, so Home cards show
                            // real creator photos without a per-channel fetch.
                            #[cfg(feature = "live")]
                            let enrich_ref: Option<&dyn YtBackend> = {
                                let r = rp.get_or_insert_with(|| try_enricher(&up_tx));
                                r.as_ref().map(|b| b as &dyn YtBackend)
                            };
                            #[cfg(not(feature = "live"))]
                            let enrich_ref: Option<&dyn YtBackend> = None;
                            match feed_trending(&live, enrich_ref, offline.as_ref(), &up_tx) {
                                Some((vs, label)) if !vs.is_empty() => {
                                    let _ = up_tx.send(Update::Feed(vs));
                                    let _ = up_tx.send(Update::Status { live: true, label });
                                }
                                _ => send_status(&up_tx, false),
                            }
                        }
                        Request::PlaylistPage(id) => {
                            // yt-dlp resolves playlists reliably; the rustypipe
                            // enricher is a fallback. Live-only — the offline build
                            // has no playlist data.
                            let mut resolved = live.as_ref().and_then(|b| b.playlist(&id).ok());
                            #[cfg(feature = "live")]
                            if resolved.is_none() {
                                let rp_ref = rp.get_or_insert_with(|| try_enricher(&up_tx));
                                if let Some(b) = rp_ref.as_ref() {
                                    resolved = b.playlist_page(&id).ok();
                                }
                            }
                            match resolved {
                                Some((pl, vids)) => {
                                    let _ = up_tx.send(Update::PlaylistPage {
                                        playlist: Box::new(pl),
                                        videos: vids,
                                    });
                                }
                                None => {
                                    let _ = up_tx
                                        .send(Update::Note(format!("playlist {id}: unavailable")));
                                }
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
                            // Real top comments — always via rustypipe, regardless of
                            // which backend answered the metadata above.
                            #[cfg(feature = "live")]
                            {
                                let rp_ref = rp.get_or_insert_with(|| try_enricher(&up_tx));
                                if let Some(b) = rp_ref.as_ref() {
                                    match b.comments(&id) {
                                        Ok(cs) if !cs.is_empty() => {
                                            let _ = up_tx.send(Update::Comments {
                                                video: id.clone(),
                                                comments: cs,
                                            });
                                        }
                                        Ok(_) => {}
                                        Err(e) => {
                                            let _ = up_tx.send(Update::Note(format!("comments: {e}")));
                                        }
                                    }
                                }
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

/// Construct the shared **rustypipe** enrichment backend. rustypipe's InnerTube
/// search returns creator avatars, playlists and creator rows the primary yt-dlp
/// path lacks, its `next`/comments continuation yields real top comments, and
/// `playlist()` resolves playlist pages — so one backend serves comments, search
/// enrichment, the avatar-bearing popular feed, and playlist pages.
#[cfg(feature = "live")]
fn try_enricher(up_tx: &Sender<Update>) -> Option<crate::backend::live::RustyPipeBackend> {
    match crate::backend::live::RustyPipeBackend::new() {
        Ok(b) => Some(b),
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!("rustypipe enrichment unavailable ({e})")));
            None
        }
    }
}

/// Fill in each video's creator avatar (yt-dlp list items carry none) from the
/// avatars rustypipe returned on the same query. Match by channel id first, then
/// by normalized creator name — the two sources sometimes key the same creator
/// with different ids (e.g. yt-dlp "Blender Studio" vs rustypipe's channel tag),
/// so name reconciliation is what fills the dominant channel's cards.
#[cfg(feature = "live")]
fn enrich_video_avatars(videos: &mut [Video], items: &crate::backend::live::SearchItems) {
    use crate::model::norm_channel_name;
    use std::collections::HashMap;
    let mut by_id: HashMap<&str, &str> = HashMap::new();
    let mut by_name: HashMap<String, &str> = HashMap::new();
    for v in &items.videos {
        if !v.channel_avatar_url.is_empty() {
            if !v.channel_id.is_empty() {
                by_id.entry(v.channel_id.as_str()).or_insert(v.channel_avatar_url.as_str());
            }
            by_name
                .entry(norm_channel_name(&v.channel_name))
                .or_insert(v.channel_avatar_url.as_str());
        }
    }
    for c in &items.channels {
        if !c.avatar_url.is_empty() {
            if !c.id.is_empty() {
                by_id.entry(c.id.as_str()).or_insert(c.avatar_url.as_str());
            }
            by_name.entry(norm_channel_name(&c.name)).or_insert(c.avatar_url.as_str());
        }
    }
    for v in videos.iter_mut() {
        if v.channel_avatar_url.is_empty() {
            if let Some(a) = by_id.get(v.channel_id.as_str()) {
                v.channel_avatar_url = a.to_string();
            } else if let Some(a) = by_name.get(&norm_channel_name(&v.channel_name)) {
                v.channel_avatar_url = a.to_string();
            }
        }
    }
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
    enrich: Option<&dyn YtBackend>,
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
    // Compose the popular feed from whichever backend carries creator avatars —
    // rustypipe when present (its items include the avatar), else the primary.
    let search_backend: Option<&dyn YtBackend> =
        enrich.or_else(|| live.as_ref().map(|b| b.as_ref()));
    live_popular_feed(search_backend, up_tx).map(|vs| (vs, "live · popular".into()))
}

/// Compose a real "popular right now" home feed: for each broad topic, ask the
/// backend for the most-viewed videos uploaded in the last week, then
/// round-robin interleave for variety and de-duplicate. Every video is genuine
/// YouTube data (view-count-sorted search), not the offline sample. Returns
/// `None` when there is no live backend so the caller keeps the sample.
fn live_popular_feed(
    backend: Option<&dyn YtBackend>,
    up_tx: &Sender<Update>,
) -> Option<Vec<Video>> {
    let backend = backend?;
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
    fn category_json_lists_all() {
        let j = category_json();
        assert!(j.contains("sponsor"));
        assert!(j.contains("music_offtopic"));
    }

    #[test]
    fn parse_skip_segments_maps_and_sorts() {
        // The shape the direct `?videoID=` endpoint returns: a flat array of
        // segment objects. Unknown categories are dropped; the rest come back
        // sorted by start time.
        let resp = serde_json::json!([
            {"category": "outro", "segment": [590.0, 600.0], "votes": 2},
            {"category": "sponsor", "segment": [30.5, 45.0], "votes": 10},
            {"category": "not_a_real_category", "segment": [1.0, 2.0], "votes": 0},
            {"category": "intro", "segment": [0.0, 12.0], "votes": 1}
        ]);
        let segs = parse_skip_segments(&resp);
        assert_eq!(segs.len(), 3, "unknown category dropped");
        assert_eq!(segs[0].category, Category::Intro);
        assert_eq!(segs[1].category, Category::Sponsor);
        assert!((segs[1].start - 30.5).abs() < 1e-9);
        assert_eq!(segs[2].category, Category::Outro);
        assert_eq!(segs[1].votes, 10);
    }

    #[test]
    fn parse_skip_segments_empty_on_non_array() {
        assert!(parse_skip_segments(&serde_json::json!({})).is_empty());
    }

    /// Real-path check: actually call SponsorBlock, DeArrow and Return YouTube
    /// Dislike and assert the parsed shapes. Honestly gated — it only touches
    /// the network when `TUBE_NET_TESTS=1`, so offline/CI runs pass trivially
    /// rather than being silently `#[ignore]`d. Run with:
    /// `TUBE_NET_TESTS=1 cargo test -p tube live_branding_apis`.
    #[test]
    fn live_branding_apis_reachable_when_enabled() {
        if std::env::var("TUBE_NET_TESTS").is_err() {
            eprintln!("skipping live branding test (set TUBE_NET_TESTS=1 to run it)");
            return;
        }
        let c = client();
        // Despacito — stable, carries locked SponsorBlock segments, DeArrow
        // branding, and a large crowdsourced dislike count.
        let vid = "kJQP7kiw5Fk";
        let segs = fetch_sponsor(&c, vid).expect("real SponsorBlock call");
        assert!(!segs.is_empty(), "Despacito has community segments");
        let (title, thumb) = fetch_dearrow(&c, vid).expect("real DeArrow call");
        assert!(
            title.is_some() || thumb.is_some(),
            "DeArrow returns community branding for Despacito"
        );
        let dislikes = fetch_dislikes(&c, vid).expect("real RYD call");
        assert!(dislikes > 0, "Despacito has crowdsourced dislikes ({dislikes})");
    }
}
