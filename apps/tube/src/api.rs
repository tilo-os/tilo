//! Live data sources: Invidious (search + metadata), `SponsorBlock`
//! (segments) and `DeArrow` (de-clickbait titles/thumbnails).
//!
//! The backends (rustypipe, yt-dlp) run on a background [`Worker`] thread;
//! the plain HTTP sources (`SponsorBlock`, `DeArrow`, dislikes) are async
//! `tilo_net` requests the worker hands to the app runtime. The `--shots` path
//! never spawns the worker, so the screenshot is populated purely from
//! `sample.rs`. The app spawns the
//! worker lazily on the first real interaction and folds results in
//! through `crate::app::TubeApp::pump`.
//!
//! Endpoints (researched, current as of 2026):
//! - Invidious: `GET {instance}/api/v1/search?q=&type=video`,
//!   `GET {instance}/api/v1/videos/{id}`
//! - `SponsorBlock`: `GET https://sponsor.ajay.app/api/skipSegments?videoID=<id>&categories=[…]`
//! - `DeArrow`: `GET https://sponsor.ajay.app/api/branding?videoID=<id>`
//! - Dislikes: `GET https://returnyoutubedislikeapi.com/votes?videoId=<id>`
//!
//! Real video playback resolves to a **fully-rangeable** proxied stream URL that
//! the shared media engine plays. [`crate::backend::proxy::resolve_rangeable`]
//! probes public Invidious instances for a progressive muxed (video+audio) MP4
//! whose `local=true` proxy serves arbitrary byte ranges over the whole file
//! (raw googlevideo URLs truncate after ~1 minute), falling back to the live
//! backend's direct [`crate::backend::YtBackend::resolve_playback_source`] when
//! no instance answers. The resolved URL comes back as [`Update::StreamResolved`]
//! and is opened into the watch page's `ui::widget::VideoPlayer` engine — which
//! does the actual `GStreamer` decode + audio out, so no decoder lives here.

use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

use crate::backend::{BackendResult, Query, YtBackend};
use crate::model::{Category, Channel, Comment, Playlist, Segment, Video};

/// Network work is deliberately a small, bounded pipeline.  A hostile source
/// or a fast sequence of keystrokes must not turn into an unbounded request or
/// UI-update backlog while the media worker is busy.
const REQUEST_QUEUE_CAPACITY: usize = 32;
const UPDATE_QUEUE_CAPACITY: usize = 128;
const MAX_UPDATES_PER_POLL: usize = 32;
const SPONSOR_ENDPOINT: &str = "https://sponsor.ajay.app/api/skipSegments";
const DEARROW_ENDPOINT: &str = "https://sponsor.ajay.app/api/branding";
const DISLIKE_ENDPOINT: &str = "https://returnyoutubedislikeapi.com/votes";

/// A unit of work for the background thread.
#[derive(Clone, Debug)]
pub enum Request {
    /// Fetch `SponsorBlock` + `DeArrow` data for a video being opened.
    Branding(String),
    /// Run a search through the data-source backend.
    Search { query: String },
    /// Fetch the home / trending catalogue. Only a *live* backend answers this;
    /// a failed or absent backend leaves the app in its explicit empty,
    /// offline state rather than substituting demo content.
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
    /// Crowdsourced dislike count for a video (Return `YouTube` Dislike).
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

/// The shared client for the plain HTTP sources; `None` only if the TLS
/// stack cannot start, which leaves those enrichments off.
fn client() -> Option<tilo_net::Client> {
    tilo_net::Client::builder()
        .user_agent("tube/0.1 (+tilo)")
        .timeout(Duration::from_secs(8))
        // In a page, sites without CORS headers are read through the host's
        // fetch proxy; the desktop never uses it.
        .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
        .build()
        .ok()
}

/// GET `endpoint?query` as JSON; `None` for a 404.
async fn get_json(
    client: &tilo_net::Client,
    endpoint: &str,
    query: &[(&str, &str)],
) -> anyhow::Result<Option<serde_json::Value>> {
    let resp = client.get(endpoint).query(query).send().await?;
    if resp.status() == tilo_net::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    Ok(Some(resp.error_for_status()?.json().await?))
}

/// Fetch `SponsorBlock` segments for a video via the direct
/// `GET /api/skipSegments?videoID=<id>&categories=[…]` endpoint. A 404 (the
/// API's "no segments for this video" response) is treated as an empty list
/// rather than an error.
///
/// # Errors
///
/// Returns `Err` if the request fails or the response body isn't valid JSON.
pub async fn fetch_sponsor(
    client: &tilo_net::Client,
    video_id: &str,
) -> anyhow::Result<Vec<Segment>> {
    fetch_sponsor_at(client, SPONSOR_ENDPOINT, video_id).await
}

async fn fetch_sponsor_at(
    client: &tilo_net::Client,
    endpoint: &str,
    video_id: &str,
) -> anyhow::Result<Vec<Segment>> {
    let cats = category_json();
    let query = [("videoID", video_id), ("categories", cats.as_str())];
    Ok(get_json(client, endpoint, &query)
        .await?
        .map(|val| parse_skip_segments(&val))
        .unwrap_or_default())
}

/// Parse a `SponsorBlock` `/api/skipSegments?videoID=<id>` response (a flat JSON
/// array of segment objects) into sorted [`Segment`]s. Pure (no I/O) so it is
/// unit-tested directly. Unknown categories and malformed rows are skipped.
pub fn parse_skip_segments(resp: &serde_json::Value) -> Vec<Segment> {
    let mut out = Vec::new();
    if let Some(arr) = resp.as_array() {
        for s in arr {
            let cat = s.get("category").and_then(|c| c.as_str()).unwrap_or("");
            let times = s.get("segment").and_then(|t| t.as_array());
            if let (Some(cat), Some(times)) = (Category::from_api_key(cat), times) {
                let start = times
                    .first()
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let end = times
                    .get(1)
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(start);
                let votes = s
                    .get("votes")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                out.push(Segment {
                    category: cat,
                    start,
                    end,
                    votes,
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.start
            .partial_cmp(&b.start)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

fn category_json() -> String {
    let names: Vec<&str> = Category::ALL.iter().map(|c| c.api_key()).collect();
    serde_json::to_string(&names).unwrap_or_else(|_| "[]".to_string())
}

/// Fetch `DeArrow` branding for a video: the top-voted crowdsourced title (when it
/// outranks the original) and a community thumbnail URL (a frame rendered at the
/// top-voted timestamp), via `GET /api/branding?videoID=<id>`.
///
/// # Errors
///
/// Returns `Err` if the request fails or the response body isn't valid JSON.
pub async fn fetch_dearrow(
    client: &tilo_net::Client,
    video_id: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    fetch_dearrow_at(client, DEARROW_ENDPOINT, video_id).await
}

async fn fetch_dearrow_at(
    client: &tilo_net::Client,
    endpoint: &str,
    video_id: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let resp = get_json(client, endpoint, &[("videoID", video_id)])
        .await?
        .ok_or_else(|| anyhow::anyhow!("HTTP 404"))?;
    Ok(parse_dearrow(&resp, video_id))
}

/// Parse a `DeArrow` `/api/branding?videoID=<id>` response into `(title,
/// thumbnail_url)`. Picks the highest-scored non-original entry (a locked entry
/// wins over vote count, matching `DeArrow`'s own precedence), and skips entries
/// the community has voted down. Pure (no I/O) so it is unit-tested directly.
pub fn parse_dearrow(resp: &serde_json::Value, video_id: &str) -> (Option<String>, Option<String>) {
    // Score an entry: locked entries win outright; otherwise by vote count.
    // Downvoted-into-removal entries (votes < 0 and not locked) are dropped.
    let score = |e: &serde_json::Value| -> Option<i64> {
        let original = e
            .get("original")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        if original {
            return None;
        }
        let votes = e
            .get("votes")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        let locked = e
            .get("locked")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
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
                .map(std::string::ToString::to_string)
        });

    let thumbnail_url = resp
        .get("thumbnails")
        .and_then(|t| t.as_array())
        .and_then(|arr| {
            arr.iter()
                .filter_map(|e| {
                    let s = score(e)?;
                    let ts = e.get("timestamp").and_then(serde_json::Value::as_f64)?;
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

/// Fetch the crowdsourced dislike count for a video via Return `YouTube` Dislike
/// (`GET /votes?videoId=<id>`).
///
/// # Errors
///
/// Returns `Err` if the request fails, the response isn't valid JSON, or it
/// carries no dislike count.
pub async fn fetch_dislikes(client: &tilo_net::Client, video_id: &str) -> anyhow::Result<u64> {
    fetch_dislikes_at(client, DISLIKE_ENDPOINT, video_id).await
}

async fn fetch_dislikes_at(
    client: &tilo_net::Client,
    endpoint: &str,
    video_id: &str,
) -> anyhow::Result<u64> {
    let resp = get_json(client, endpoint, &[("videoId", video_id)])
        .await?
        .ok_or_else(|| anyhow::anyhow!("HTTP 404"))?;
    parse_dislikes(&resp).ok_or_else(|| anyhow::anyhow!("no dislikes field in RYD response"))
}

/// Parse a Return `YouTube` Dislike `/votes` response's dislike count. Pure (no
/// I/O) so it is unit-tested directly.
pub fn parse_dislikes(resp: &serde_json::Value) -> Option<u64> {
    resp.get("dislikes").and_then(serde_json::Value::as_u64)
}

/// Minimal Invidious search → [`Video`] mapping. Best-effort; missing
/// fields fall back to sensible defaults.
///
/// # Errors
///
/// Returns `Err` if the request fails or the response body isn't valid JSON.
pub async fn search(
    client: &tilo_net::Client,
    instance: &str,
    query: &str,
) -> anyhow::Result<Vec<Video>> {
    let url = format!("{}/api/v1/search", instance.trim_end_matches('/'));
    let resp = get_json(client, &url, &[("q", query), ("type", "video")])
        .await?
        .ok_or_else(|| anyhow::anyhow!("HTTP 404"))?;

    Ok(resp
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(40)
                .filter_map(|item| video_from_item(item, "Search"))
                .collect()
        })
        .unwrap_or_default())
}

/// One Invidious list item (a search hit, a trending or recommended video,
/// a channel upload) as a [`Video`]; `None` for other kinds and id-less rows.
fn video_from_item(item: &serde_json::Value, category: &str) -> Option<Video> {
    if item
        .get("type")
        .and_then(|t| t.as_str())
        .is_some_and(|kind| kind != "video")
    {
        return None;
    }
    let id = item
        .get("videoId")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    if id.is_empty() {
        return None;
    }
    let title = item
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled");
    let author = item
        .get("author")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");
    let author_id = item
        .get("authorId")
        .and_then(|v| v.as_str())
        .unwrap_or(author);
    let views = item
        .get("viewCount")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let duration = item
        .get("lengthSeconds")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    let published = item
        .get("publishedText")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some(Video {
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
            .map(std::string::ToString::to_string)
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
            .map(std::string::ToString::to_string)
            .unwrap_or_default(),
        category: category.to_string(),
        dearrow_thumb: false,
        sponsor: vec![],
        comments: vec![],
        live: item
            .get("liveNow")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
    })
}

/// Fetch one video's `SponsorBlock` segments, `DeArrow` branding and dislike
/// count, posting each as it arrives.
async fn branding(client: tilo_net::Client, id: String, up_tx: SyncSender<Update>) {
    match fetch_sponsor(&client, &id).await {
        Ok(segs) => {
            let _ = up_tx.send(Update::Sponsor(id.clone(), segs));
        }
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!("SponsorBlock: {e}")));
        }
    }
    match fetch_dearrow(&client, &id).await {
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
    match fetch_dislikes(&client, &id).await {
        Ok(count) => {
            let _ = up_tx.send(Update::Dislikes { video: id, count });
        }
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!("Dislikes: {e}")));
        }
    }
}

/// The background worker: drives the video backends and turns [`Request`]s
/// into [`Update`]s.
pub struct Worker {
    tx: SyncSender<Request>,
    rx: Receiver<Update>,
}

impl Worker {
    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per Request variant in the worker's dispatch loop; the \
                  length is the number of request kinds, not accidental complexity in any \
                  one arm"
    )]
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<Request>(REQUEST_QUEUE_CAPACITY);
        let (up_tx, up_rx) = std::sync::mpsc::sync_channel::<Update>(UPDATE_QUEUE_CAPACITY);
        // Without threads (a browser page) the extractors cannot run; the
        // public Invidious API answers instead, as tasks on the app runtime.
        if matches!(
            ui::rt::availability(ui::rt::Feature::Threads),
            ui::rt::Availability::Unsupported(_)
        ) {
            if let Ok(runtime) = ui::rt::try_app() {
                drop(runtime.spawn(task_worker(req_rx, up_tx)));
            }
            return Self {
                tx: req_tx,
                rx: up_rx,
            };
        }
        // If the OS can't give us a thread (resource exhaustion), degrade to a
        // worker that never produces updates rather than crashing the app:
        // `request` sends go unread and `poll` stays empty, same as "no live
        // data yet".
        if let Err(e) = ui::rt::spawn_thread("tube-fetch", move || {
            let client = client();
            // The offline backend supplies hermetic test/demo data only; the
            // live app never falls back to it, so a failed network request
            // cannot present fabricated videos as current content. The live
            // rustypipe backend is present only under `--features live` *and*
            // if it constructed. SponsorBlock /
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
                        if let (Some(client), Ok(runtime)) = (&client, ui::rt::try_app()) {
                            drop(runtime.spawn(branding(client.clone(), id, up_tx.clone())));
                        }
                    }
                    Request::Search { query } => {
                        // Live only — NEVER fall back to the bundled sample:
                        // presenting fabricated videos as real search results
                        // is exactly what the "no fake content" rule forbids.
                        // Always emit a (possibly empty) result so the UI can
                        // drop its loading skeleton and show either real hits
                        // or an honest "no results".
                        #[allow(
                            unused_mut,
                            reason = "live-only enrichment mutates this result list"
                        )]
                        let mut videos = match fetch(
                            live.as_deref(),
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
                        #[allow(
                            unused_mut,
                            reason = "live-only enrichment fills these result lists"
                        )]
                        let mut playlists: Vec<Playlist> = Vec::new();
                        #[allow(
                            unused_mut,
                            reason = "live-only enrichment fills these result lists"
                        )]
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
                                        let _ =
                                            up_tx.send(Update::Note(format!("search enrich: {e}")));
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
                        // Live only: a missing/failed backend marks the source
                        // offline and leaves the intentionally empty catalogue
                        // in its explicit offline state.
                        //
                        // First choice is YouTube's dedicated trending browse.
                        // That endpoint (`FEtrending`) has been retired and now
                        // answers HTTP 400 "Request contains an invalid
                        // argument."; there is no no-login browse that returns a
                        // general trending feed. When it fails we compose a
                        // "popular right now" feed from view-count-sorted,
                        // last-week search across broad topics — YouTube data,
                        // never the offline sample.
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
                        match feed_trending(live.as_deref(), enrich_ref, offline.as_ref(), &up_tx) {
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
                        #[allow(
                            unused_mut,
                            reason = "the live-only enricher is a playlist fallback"
                        )]
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
                                let _ =
                                    up_tx.send(Update::Note(format!("playlist {id}: unavailable")));
                            }
                        }
                    }
                    Request::VideoMeta(id) => {
                        // Real "up next" comes off the detail response; fetch
                        // it alongside the metadata (live only — the sample
                        // already carries full detail for its own ids).
                        let related = match fetch(
                            live.as_deref(),
                            offline.as_ref(),
                            Query::Related(id.clone()),
                            false,
                            &up_tx,
                        ) {
                            Some((BackendResult::Videos(r), _)) => r,
                            _ => Vec::new(),
                        };
                        if let Some((BackendResult::Video(v), used_live)) = fetch(
                            live.as_deref(),
                            offline.as_ref(),
                            Query::Video(id.clone()),
                            false,
                            &up_tx,
                        ) {
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
                            fetch(
                                live.as_deref(),
                                offline.as_ref(),
                                Query::Channel(id),
                                false,
                                &up_tx,
                            )
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
                        match resolve_stream_url(live.as_deref(), &id) {
                            Some((url, has_audio, quality)) => {
                                let _ = up_tx.send(Update::Note(format!(
                                    "stream: {quality} {} [{}]",
                                    if has_audio {
                                        "video+audio"
                                    } else {
                                        "video only"
                                    },
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
        }) {
            tracing::error!("failed to spawn the tube-fetch worker thread: {e}");
        }
        Self {
            tx: req_tx,
            rx: up_rx,
        }
    }

    /// Queue work without ever blocking the UI thread.
    ///
    /// # Errors
    ///
    /// Returns the unqueued request when the bounded worker is saturated or
    /// has stopped. Callers must surface this outcome; a one-shot navigation,
    /// search, or playback intent is never silently discarded.
    pub fn request(&self, req: Request) -> Result<(), std::sync::mpsc::TrySendError<Request>> {
        self.tx.try_send(req)
    }

    /// Non-blocking drain of any results ready this frame.
    pub fn poll(&self) -> Vec<Update> {
        self.rx.try_iter().take(MAX_UPDATES_PER_POLL).collect()
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
fn resolve_stream_url(live: Option<&dyn YtBackend>, id: &str) -> Option<(String, bool, String)> {
    // Primary: yt-dlp resolves a full, non-SABR-gated googlevideo URL (via the
    // `android_vr` client) that streams the WHOLE video+audio without the
    // ~1-minute truncation that gates raw iOS/TV URLs — so the media engine can
    // play the full length and seek. (Proven with `--probe-engine`: full
    // duration, buffered to 100%.)
    if let Some(b) = live
        && let Ok(src) = b.resolve_playback_source(id)
        && !src.url.is_empty()
    {
        return Some((src.url, src.has_audio, src.quality));
    }
    // Fallback: a fully-rangeable Invidious `local=true` proxy. The public
    // instance network has largely collapsed, so this is a last resort for when
    // yt-dlp is unavailable.
    let proxied = ui::rt::block_on(crate::backend::proxy::resolve_rangeable(id))
        .ok()
        .flatten();
    if let Some(r) = proxied {
        return Some((r.url, r.has_audio, r.quality));
    }
    None
}

/// Construct the live rustypipe backend when the `live` feature is on and it
/// initialises. Returns `None` in the default build or on a construction error
/// (the UI then shows its explicit offline/no-data state). Never panics.
#[cfg(feature = "live")]
fn try_live_backend(up_tx: &SyncSender<Update>) -> Option<Box<dyn YtBackend>> {
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
            let _ = up_tx.send(Update::Note(format!(
                "live backend ready (fallback): {}",
                b.name()
            )));
            Some(Box::new(b))
        }
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!(
                "live backend unavailable ({e}); no live catalogue available"
            )));
            None
        }
    }
}

#[cfg(not(feature = "live"))]
fn try_live_backend(_up_tx: &SyncSender<Update>) -> Option<Box<dyn YtBackend>> {
    None
}

/// Construct the shared **rustypipe** enrichment backend. rustypipe's `InnerTube`
/// search returns creator avatars, playlists and creator rows the primary yt-dlp
/// path lacks, its `next`/comments continuation yields real top comments, and
/// `playlist()` resolves playlist pages — so one backend serves comments, search
/// enrichment, the avatar-bearing popular feed, and playlist pages.
#[cfg(feature = "live")]
fn try_enricher(up_tx: &SyncSender<Update>) -> Option<crate::backend::live::RustyPipeBackend> {
    match crate::backend::live::RustyPipeBackend::new() {
        Ok(b) => Some(b),
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!(
                "rustypipe enrichment unavailable ({e})"
            )));
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
                by_id
                    .entry(v.channel_id.as_str())
                    .or_insert(v.channel_avatar_url.as_str());
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
            by_name
                .entry(norm_channel_name(&c.name))
                .or_insert(c.avatar_url.as_str());
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

/// Run `query` against the live backend first (when present); callers may opt
/// into the offline backend only for hermetic/demo use. The app passes
/// `false`, so it never represents sample data as current content. Returns the
/// result together with whether the *live* backend produced it. Live errors are
/// surfaced as a status note.
fn fetch(
    live: Option<&dyn YtBackend>,
    offline: &dyn YtBackend,
    query: Query,
    fallback: bool,
    up_tx: &SyncSender<Update>,
) -> Option<(BackendResult, bool)> {
    if let Some(b) = live {
        match b.run(query.clone()) {
            Ok(r) => return Some((r, true)),
            Err(e) => {
                let _ = up_tx.send(Update::Note(format!("live {query:?}: {e}")));
            }
        }
    }
    if fallback && let Ok(r) = offline.run(query) {
        return Some((r, false));
    }
    None
}

/// Resolve the Home/Trending catalogue from live data, returning the videos and
/// the status-pill label. Tries `YouTube`'s dedicated trending browse first; on
/// failure (the endpoint has been retired) composes a "popular right now" feed.
/// Returns `None` only when there is no live data at all
/// (offline / default build) so the caller keeps its explicit empty state.
fn feed_trending(
    live: Option<&dyn YtBackend>,
    enrich: Option<&dyn YtBackend>,
    offline: &dyn YtBackend,
    up_tx: &SyncSender<Update>,
) -> Option<(Vec<Video>, String)> {
    // Dedicated endpoint first. `fetch` surfaces the error as a status note
    // (e.g. "live Trending: trending: http status 400"), so the failure of the
    // retired endpoint is visible rather than silently swallowed.
    if let Some((BackendResult::Videos(vs), true)) =
        fetch(live, offline, Query::Trending, false, up_tx)
        && !vs.is_empty()
    {
        return Some((vs, "live · trending".into()));
    }
    // Compose the popular feed from whichever backend carries creator avatars —
    // rustypipe when present (its items include the avatar), else the primary.
    let search_backend: Option<&dyn YtBackend> = enrich.or(live);
    live_popular_feed(search_backend, up_tx).map(|vs| (vs, "live · popular".into()))
}

/// Compose a "popular right now" home feed: for each broad topic, ask the
/// backend for the most-viewed videos uploaded in the last week, then
/// round-robin interleave for variety and de-duplicate. Every video comes from
/// `YouTube` data (view-count-sorted search), not the offline sample. Returns
/// `None` when there is no live backend so the caller keeps the empty state.
fn live_popular_feed(
    backend: Option<&dyn YtBackend>,
    up_tx: &SyncSender<Update>,
) -> Option<Vec<Video>> {
    // Broad, evergreen topics so the union approximates a general popular feed
    // rather than one niche.
    const SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];
    let backend = backend?;
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
    videos
        .into_iter()
        .filter(|v| seen.insert(v.id.clone()))
        .collect()
}

/// Broadcast which data source just answered so the UI pill reflects reality.
/// The worker where there are no threads: every request answered from the
/// public Invidious API through `tilo_net` (the host's fetch proxy where a
/// site refuses cross-origin reads). Updates are offered, never waited on:
/// the page has one thread and the UI drains them.
#[allow(
    clippy::too_many_lines,
    reason = "one match arm per Request variant, as in the threaded worker's loop"
)]
async fn task_worker(requests: Receiver<Request>, updates: SyncSender<Update>) {
    let post = |update: Update| {
        let _ = updates.try_send(update);
    };
    let Some(client) = client() else {
        post(Update::Status {
            live: false,
            label: "offline — no network client".into(),
        });
        return;
    };
    post(Update::Status {
        live: false,
        label: "connecting…".into(),
    });
    loop {
        let request = match requests.try_recv() {
            Ok(request) => request,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                ui::rt::sleep(Duration::from_millis(40)).await;
                continue;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
        };
        match request {
            Request::Branding(id) => branding(client.clone(), id, updates.clone()).await,
            Request::Search { query } => {
                let found = invidious_videos(
                    &client,
                    "/api/v1/search",
                    &[("q", query.as_str()), ("type", "video")],
                    "Search",
                )
                .await;
                post(invidious_status(found.is_ok()));
                post(Update::SearchResults {
                    query,
                    videos: found.unwrap_or_default(),
                    playlists: Vec::new(),
                    channels: Vec::new(),
                });
            }
            Request::Feed => {
                match invidious_videos(&client, "/api/v1/trending", &[], "Trending").await {
                    Ok(videos) if !videos.is_empty() => {
                        post(Update::Feed(videos));
                        post(invidious_status(true));
                    }
                    _ => post(invidious_status(false)),
                }
            }
            Request::VideoMeta(id) => {
                let path = format!("/api/v1/videos/{id}");
                if let Ok(detail) = invidious_json(&client, &path, &[]).await
                    && let Some(video) = video_from_item(&detail, "Video")
                {
                    let related = detail
                        .get("recommendedVideos")
                        .and_then(|items| items.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| video_from_item(item, "Related"))
                                .collect()
                        })
                        .unwrap_or_default();
                    post(Update::VideoMeta {
                        video: Box::new(video),
                        related,
                    });
                }
            }
            Request::Channel(id) => {
                let path = format!("/api/v1/channels/{id}");
                if let Ok(page) = invidious_json(&client, &path, &[]).await {
                    let text = |key: &str| {
                        page.get(key)
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    };
                    let image = |key: &str| {
                        page.get(key)
                            .and_then(|v| v.as_array())
                            .and_then(|images| images.last())
                            .and_then(|image| image.get("url"))
                            .and_then(|url| url.as_str())
                            .unwrap_or_default()
                            .to_string()
                    };
                    let channel = Channel {
                        id: id.clone(),
                        name: text("author"),
                        handle: String::new(),
                        subscribers: page
                            .get("subCount")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        verified: page
                            .get("authorVerified")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false),
                        description: text("description"),
                        seed: id.clone(),
                        avatar_url: image("authorThumbnails"),
                        banner_url: image("authorBanners"),
                    };
                    let uploads = page
                        .get("latestVideos")
                        .and_then(|items| items.as_array())
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| video_from_item(item, "Uploads"))
                                .collect()
                        })
                        .unwrap_or_default();
                    post(Update::ChannelPage {
                        channel: Box::new(channel),
                        uploads,
                    });
                }
            }
            Request::PlaylistPage(id) => {
                post(Update::Note(format!("playlist {id}: unavailable here")));
            }
            Request::PlayVideo(id) => match crate::backend::proxy::resolve_rangeable(&id).await {
                Some(resolved) => post(Update::StreamResolved {
                    video: id,
                    url: resolved.url,
                    has_audio: resolved.has_audio,
                    quality: resolved.quality,
                }),
                None => post(Update::Note(
                    "could not resolve a playable stream (no Invidious instance served bytes)"
                        .into(),
                )),
            },
        }
    }
}

fn invidious_status(live: bool) -> Update {
    Update::Status {
        live,
        label: if live {
            "live (Invidious)".into()
        } else {
            "offline — no Invidious instance answered".into()
        },
    }
}

/// `path` (with `query`) as JSON from the first public Invidious instance
/// that answers it.
async fn invidious_json(
    client: &tilo_net::Client,
    path: &str,
    query: &[(&str, &str)],
) -> anyhow::Result<serde_json::Value> {
    let mut last = anyhow::anyhow!("no Invidious instance answered");
    for instance in crate::backend::proxy::instances() {
        let url = format!("{}{path}", instance.trim_end_matches('/'));
        match get_json(client, &url, query).await {
            Ok(Some(value)) if value.get("error").is_none() => return Ok(value),
            Ok(_) => last = anyhow::anyhow!("{instance} has no answer for {path}"),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// A list of videos (search hits, trending) from the Invidious API.
async fn invidious_videos(
    client: &tilo_net::Client,
    path: &str,
    query: &[(&str, &str)],
    category: &str,
) -> anyhow::Result<Vec<Video>> {
    let value = invidious_json(client, path, query).await?;
    Ok(value
        .as_array()
        .map(|items| {
            items
                .iter()
                .take(60)
                .filter_map(|item| video_from_item(item, category))
                .collect()
        })
        .unwrap_or_default())
}

fn send_status(up_tx: &SyncSender<Update>, live: bool) {
    let label = if live {
        "live (rustypipe)"
    } else {
        "offline — no live data"
    };
    let _ = up_tx.send(Update::Status {
        live,
        label: label.into(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        ui::rt::app()
            .block_on(future)
            .expect("the app runtime runs")
    }

    fn serve_http(
        responses: Vec<(&'static str, u16, &'static str)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local HTTP fixture");
        let address = listener.local_addr().expect("read fixture address");
        let handle = std::thread::spawn(move || {
            for (expected_target, status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept fixture request");
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).expect("read fixture request");
                    assert!(count > 0, "client closed before sending HTTP headers");
                    request.extend_from_slice(&chunk[..count]);
                    assert!(
                        request.len() < 16 * 1024,
                        "fixture request headers are bounded"
                    );
                }
                let request = String::from_utf8(request).expect("request headers are UTF-8");
                let request_line = request.lines().next().expect("request line");
                assert!(
                    request_line.starts_with(&format!("GET {expected_target}")),
                    "unexpected request target: {request_line}"
                );
                if expected_target.ends_with("categories=") {
                    assert!(
                        !request_line.starts_with(&format!("GET {expected_target} HTTP/")),
                        "SponsorBlock categories query must be populated"
                    );
                }
                let reason = if status == 200 { "OK" } else { "Not Found" };
                write!(
                    stream,
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .expect("write fixture response");
            }
        });
        (format!("http://{address}"), handle)
    }

    #[test]
    fn sponsorblock_categories_and_segments_parse() {
        // Case `category_json_lists_all`.
        {
            let j = category_json();
            assert!(j.contains("sponsor"));
            assert!(j.contains("music_offtopic"));
        }
        // Case `parse_skip_segments_maps_and_sorts`.
        {
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
        // Case `parse_skip_segments_empty_on_non_array`.
        {
            assert!(parse_skip_segments(&serde_json::json!({})).is_empty());
        }
    }

    #[test]
    fn api_workers_queue_and_query_branding_endpoints() {
        // Case `saturated_worker_queue_returns_the_unqueued_request`.
        {
            let (tx, _hold_queue) = std::sync::mpsc::sync_channel(1);
            let (_unused_tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker = Worker { tx, rx };
            assert!(worker.request(Request::Feed).is_ok());
            let error = worker
                .request(Request::Search {
                    query: "latest intent".into(),
                })
                .expect_err("saturation must be observable to the app");
            let rejected = match error {
                std::sync::mpsc::TrySendError::Full(request)
                | std::sync::mpsc::TrySendError::Disconnected(request) => request,
            };
            assert!(matches!(
                rejected,
                Request::Search { query } if query == "latest intent"
            ));
        }
        // Case `branding_clients_send_expected_queries_and_parse_http_responses`.
        {
            let (base, server) = serve_http(vec![
                (
                    "/api/skipSegments?videoID=fixture&categories=",
                    200,
                    r#"[{"category":"outro","segment":[9.0,10.0],"votes":2},{"category":"sponsor","segment":[1.0,3.0],"votes":7}]"#,
                ),
                (
                    "/api/branding?videoID=fixture",
                    200,
                    r#"{"titles":[{"title":"Accurate title","votes":5,"original":false}],"thumbnails":[{"timestamp":12.5,"votes":4,"original":false}]}"#,
                ),
                ("/votes?videoId=fixture", 200, r#"{"dislikes":42}"#),
                ("/api/skipSegments?videoID=none&categories=", 404, ""),
            ]);
            let c = client().expect("an HTTP client");
            let segments = block(fetch_sponsor_at(
                &c,
                &format!("{base}/api/skipSegments"),
                "fixture",
            ))
            .expect("SponsorBlock response");
            assert_eq!(segments.len(), 2);
            assert_eq!(segments[0].category, Category::Sponsor);
            assert_eq!(segments[0].votes, 7);

            let (title, thumbnail) = block(fetch_dearrow_at(
                &c,
                &format!("{base}/api/branding"),
                "fixture",
            ))
            .expect("DeArrow response");
            assert_eq!(title.as_deref(), Some("Accurate title"));
            assert!(thumbnail.is_some_and(|url| url.contains("time=12.5")));
            assert_eq!(
                block(fetch_dislikes_at(&c, &format!("{base}/votes"), "fixture"))
                    .expect("dislike response"),
                42
            );
            assert!(
                block(fetch_sponsor_at(
                    &c,
                    &format!("{base}/api/skipSegments"),
                    "none"
                ))
                .expect("404 means no segments")
                .is_empty()
            );
            server.join().expect("HTTP fixture completed");
        }
    }
}
