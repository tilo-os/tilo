//! Live data sources: `YouTube` through rustypipe ([`crate::backend::live`])
//! where it runs, its own web API ([`crate::innertube`]) in a browser page,
//! where rustypipe cannot; `SponsorBlock` (segments), `DeArrow`
//! (de-clickbait titles/thumbnails) and Return `YouTube` Dislike beside it.
//!
//! The rustypipe backend runs on a background [`Worker`] thread; the plain
//! HTTP sources are async `tilo_net` requests the worker hands to the app
//! runtime. The `--shots` path never spawns the worker, so the screenshot is
//! populated purely from `sample.rs`. The app spawns the worker lazily on the
//! first real interaction and folds results in through
//! `crate::app::TubeApp::pump`.
//!
//! Endpoints (researched, current as of 2026):
//! - `SponsorBlock`: `GET https://sponsor.ajay.app/api/skipSegments?videoID=<id>&categories=[…]`
//! - `DeArrow`: `GET https://sponsor.ajay.app/api/branding?videoID=<id>`
//! - Dislikes: `GET https://returnyoutubedislikeapi.com/votes?videoId=<id>`
//!
//! Playback resolves to a whole-file progressive stream (picture and sound in
//! one file) that serves every byte range, so the shared media engine can
//! seek and play to the end. It comes back as [`Update::StreamResolved`] and
//! is opened into the watch page's `ui::widget::VideoPlayer` engine, which
//! does the actual `GStreamer` decode and audio out.

use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

#[cfg(feature = "live")]
use crate::backend::{BackendResult, YtBackend};
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
    /// thread, since it asks `YouTube` over the network. The
    /// resolved URL comes back as [`Update::StreamResolved`]; the actual decode +
    /// audio out happens in the watch page's media-engine-backed `VideoPlayer`,
    /// not here.
    PlayVideo {
        id: String,
        /// The quality the person chose (`"720p"`, `"auto"`): the stream
        /// resolved is no larger than it.
        quality: String,
    },
    /// Fetch the caption tracks `id` offers, and then the track the person
    /// chose, if there is one.
    Captions {
        id: String,
        /// The language code of the track to download, or `None` to fetch
        /// only the list of tracks the video offers.
        language: Option<String>,
    },
}

/// The worker's end of the result queue. Every result it posts wakes the
/// window that asked for it, so a reply the network sends while nobody
/// touches the app is shown at once: the window drains the queue only when it
/// runs a frame, and without the wake it ran none until the next input or
/// animation.
#[derive(Clone)]
pub struct Updates {
    tx: SyncSender<Update>,
    wake: ui::WakeHandle,
}

impl Updates {
    /// Post `update`, waiting while the queue is full, and wake the window.
    pub fn send(&self, update: Update) -> Result<(), std::sync::mpsc::SendError<Update>> {
        self.tx.send(update)?;
        self.wake.wake();
        Ok(())
    }

    /// Post `update` unless the queue is full, and wake the window. The
    /// refusal is boxed: it carries the whole update back.
    pub fn try_send(
        &self,
        update: Update,
    ) -> Result<(), Box<std::sync::mpsc::TrySendError<Update>>> {
        self.tx.try_send(update).map_err(Box::new)?;
        self.wake.wake();
        Ok(())
    }
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
    /// Crowdsourced dislike count for a video (Return `YouTube` Dislike),
    /// with the like count it reports beside it: the watch page shows both
    /// counts or neither, and a search hit carries no like count of its own.
    Dislikes {
        video: String,
        count: u64,
        likes: Option<u64>,
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
    /// Which data source is currently answering (drives the header status badge).
    Status {
        live: bool,
        label: String,
    },
    /// No source answered a feed or search request, and why. Settles the
    /// request so the page stops loading and says what is wrong.
    Unreached(Unreached),
    /// A real, directly-playable stream URL resolved for a watch id. The UI
    /// opens it into the watch page's `VideoPlayer` engine (real decode + audio).
    StreamResolved {
        video: String,
        url: String,
        has_audio: bool,
        /// Short quality label for the status log (e.g. `360p`).
        quality: String,
    },
    /// The caption tracks a video offers, and the lines of one of them when
    /// the person chose it. `lines` is `None` when only the list was asked
    /// for; `failure` says why a chosen track could not be read.
    Captions {
        video: String,
        tracks: Vec<crate::captions::CaptionTrack>,
        language: Option<String>,
        lines: Option<Vec<crate::captions::CaptionCue>>,
        failure: Option<String>,
    },
    /// No source gave a playable stream for a watch id: the page says so
    /// rather than showing its still poster as if it were about to play.
    Unplayable {
        video: String,
        /// `YouTube`'s own words when it refused to play the video (such
        /// as a bot check), for the page to pass on.
        said: Option<String>,
    },
    /// The top videos `YouTube` answers for what the platform search is
    /// asked while the person types ([`crate::suggest`]); empty when no
    /// source answered.
    Suggestions {
        query: String,
        videos: Vec<Video>,
    },
    /// Non-fatal note surfaced in the status log.
    Note(String),
}

/// Why a live request found no source. Only [`Unreached::Offline`] means the
/// device has no network: a source that refuses, fails or cannot be read
/// while the device is online is a different problem with a different fix,
/// and calling it "offline" sends the person to check a connection that
/// works.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreached {
    /// The device has no network.
    Offline,
    /// The app runs in a browser page, the sources do not let a page read
    /// their responses (CORS), and the host serving the page offers no fetch
    /// proxy to relay them.
    NoRelay,
    /// The network is up but no source answered; the last failure.
    Failed(String),
}

impl Unreached {
    /// Classify a failed request. `online` is the device's network status,
    /// `relay` whether the host offers a fetch proxy, and `unreadable`
    /// whether no source gave a readable answer and the browser hid at least
    /// one, as it hides a cross-origin refusal.
    #[must_use]
    pub fn classify(online: bool, relay: bool, unreadable: bool, last: String) -> Self {
        if !online {
            Self::Offline
        } else if unreadable && !relay {
            Self::NoRelay
        } else {
            Self::Failed(last)
        }
    }

    /// [`Self::classify`] against this device's status and host.
    fn now(unreadable: bool, last: String) -> Self {
        Self::classify(
            tilo_net::status::online(),
            tilo_net::proxy::endpoint().is_some(),
            unreadable,
            last,
        )
    }
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

/// A video's votes as Return `YouTube` Dislike reports them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Votes {
    /// The crowdsourced dislike estimate.
    pub dislikes: u64,
    /// The like count, when the response carries one.
    pub likes: Option<u64>,
}

/// Fetch a video's votes via Return `YouTube` Dislike
/// (`GET /votes?videoId=<id>`).
pub async fn fetch_votes(client: &tilo_net::Client, video_id: &str) -> anyhow::Result<Votes> {
    fetch_votes_at(client, DISLIKE_ENDPOINT, video_id).await
}

async fn fetch_votes_at(
    client: &tilo_net::Client,
    endpoint: &str,
    video_id: &str,
) -> anyhow::Result<Votes> {
    let resp = get_json(client, endpoint, &[("videoId", video_id)])
        .await?
        .ok_or_else(|| anyhow::anyhow!("HTTP 404"))?;
    parse_votes(&resp).ok_or_else(|| anyhow::anyhow!("no dislikes field in RYD response"))
}

/// Parse a Return `YouTube` Dislike `/votes` response. Pure (no I/O) so it
/// is unit-tested directly.
pub fn parse_votes(resp: &serde_json::Value) -> Option<Votes> {
    Some(Votes {
        dislikes: resp.get("dislikes")?.as_u64()?,
        likes: resp.get("likes").and_then(serde_json::Value::as_u64),
    })
}

/// Fetch one video's `SponsorBlock` segments, `DeArrow` branding and dislike
/// count, posting each as it arrives.
async fn branding(client: tilo_net::Client, id: String, up_tx: Updates) {
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
    match fetch_votes(&client, &id).await {
        Ok(votes) => {
            let _ = up_tx.send(Update::Dislikes {
                video: id,
                count: votes.dislikes,
                likes: votes.likes,
            });
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
    /// Where the platform search's live answers are asked from; `None` for
    /// a scripted worker, whose answers the test sends.
    suggest: Option<(crate::suggest::Suggesting, Updates)>,
}

impl Worker {
    pub fn spawn(wake: ui::WakeHandle) -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::sync_channel::<Request>(REQUEST_QUEUE_CAPACITY);
        let (up_tx, up_rx) = std::sync::mpsc::sync_channel::<Update>(UPDATE_QUEUE_CAPACITY);
        let up_tx = Updates { tx: up_tx, wake };
        let suggest = Some((crate::suggest::Suggesting::default(), up_tx.clone()));
        #[cfg(feature = "live")]
        {
            // If the OS can't give us a thread (resource exhaustion), degrade
            // to a worker that never produces updates rather than crashing the
            // app: `request` sends go unread and `poll` stays empty, same as
            // "no live data yet".
            if let Err(e) = ui::rt::spawn_thread("tube-fetch", move || live_worker(&req_rx, &up_tx))
            {
                tracing::error!("failed to spawn the tube-fetch worker thread: {e}");
            }
        }
        // A build without rustypipe (a browser page, where it cannot run)
        // answers from `YouTube`'s web API as tasks on the app runtime.
        #[cfg(not(feature = "live"))]
        if let Ok(runtime) = ui::rt::try_app() {
            drop(runtime.spawn(task_worker(req_rx, up_tx)));
        }
        Self {
            tx: req_tx,
            rx: up_rx,
            suggest,
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

    /// Ask `YouTube` for the top videos of `query`, what the platform
    /// search is asked, answered as [`Update::Suggestions`]. Asks on the app
    /// runtime, apart from the request queue, so a slow request never holds
    /// it up; one request at a time, for the latest query once the typing
    /// pauses ([`crate::suggest`]). False when nothing will answer (no
    /// runtime to ask on), so the search does not wait for it.
    pub fn suggest(&self, query: &str) -> bool {
        let Some((suggesting, updates)) = &self.suggest else {
            // Scripted: the test sends the answers.
            return true;
        };
        if !suggesting.want(query) {
            return true;
        }
        let Ok(runtime) = ui::rt::try_app() else {
            suggesting.give_up();
            return false;
        };
        let (suggesting, updates) = (suggesting.clone(), updates.clone());
        // Pinned: rustypipe's query futures stay on the thread that polls them.
        drop(runtime.spawn_pinned(move || async move {
            #[cfg(feature = "live")]
            let fetch =
                |query: String| async move { crate::backend::live::suggested_videos(&query).await };
            #[cfg(not(feature = "live"))]
            let youtube = client().map(crate::innertube::InnerTube::new);
            #[cfg(not(feature = "live"))]
            let fetch = |query: String| {
                let youtube = youtube.as_ref();
                async move {
                    match youtube {
                        Some(youtube) => youtube
                            .search(&query)
                            .await
                            .map(|found| found.videos)
                            .unwrap_or_default(),
                        None => Vec::new(),
                    }
                }
            };
            crate::suggest::ask_youtube(suggesting, fetch, |update| {
                let _ = updates.try_send(update);
            })
            .await;
        }));
        true
    }

    /// Non-blocking drain of any results ready this frame.
    pub fn poll(&self) -> Vec<Update> {
        self.rx.try_iter().take(MAX_UPDATES_PER_POLL).collect()
    }

    /// A worker that fetches nothing: its results are what the returned
    /// sender sends. For a test that lands a result at a moment it chooses.
    #[doc(hidden)]
    #[must_use]
    pub fn scripted(wake: ui::WakeHandle) -> (Self, Updates) {
        let (tx, _requests) = std::sync::mpsc::sync_channel::<Request>(REQUEST_QUEUE_CAPACITY);
        let (results, rx) = std::sync::mpsc::sync_channel::<Update>(UPDATE_QUEUE_CAPACITY);
        (
            Self {
                tx,
                rx,
                suggest: None,
            },
            Updates { tx: results, wake },
        )
    }
}

/// The threaded worker of a build with rustypipe: every `YouTube` request
/// goes to the one rustypipe backend. The offline sample is hermetic test
/// data only; a failed request is reported, never answered with fabricated
/// videos. `SponsorBlock`, `DeArrow` and the dislike count keep their own
/// HTTP sources.
#[cfg(feature = "live")]
fn live_worker(requests: &Receiver<Request>, up_tx: &Updates) {
    let client = client();
    let live = match crate::backend::live::RustyPipeBackend::new() {
        Ok(backend) => Some(backend),
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!(
                "live backend unavailable ({e}); no live catalogue available"
            )));
            None
        }
    };
    // Construction doesn't touch the network, so "live" waits for a real
    // answer.
    let _ = up_tx.send(Update::Status {
        live: false,
        label: if live.is_some() {
            "connecting…".into()
        } else {
            "no live source".into()
        },
    });
    while let Ok(request) = requests.recv() {
        if let Request::Branding(id) = request {
            if let (Some(client), Ok(runtime)) = (&client, ui::rt::try_app()) {
                drop(runtime.spawn(branding(client.clone(), id, up_tx.clone())));
            }
            continue;
        }
        match &live {
            Some(live) => answer_live(live, client.as_ref(), request, up_tx),
            None => answer_unreached(request, up_tx),
        }
    }
}

/// Answer one request from rustypipe.
#[cfg(feature = "live")]
fn answer_live(
    live: &crate::backend::live::RustyPipeBackend,
    client: Option<&tilo_net::Client>,
    request: Request,
    up_tx: &Updates,
) {
    let note = |what: String| {
        let _ = up_tx.send(Update::Note(what));
    };
    match request {
        Request::Branding(_) => {}
        Request::Search { query } => {
            // Always emit a (possibly empty) result so the UI drops its
            // loading skeleton and shows real hits or an honest "no results".
            let found = live.search_items(&query);
            if found.is_ok() {
                send_live_status(up_tx);
            }
            let found = found.unwrap_or_else(|e| {
                note(format!("search: {e}"));
                crate::backend::live::SearchItems {
                    videos: Vec::new(),
                    playlists: Vec::new(),
                    channels: Vec::new(),
                }
            });
            let _ = up_tx.send(Update::SearchResults {
                query,
                videos: found.videos,
                playlists: found.playlists,
                channels: found.channels,
            });
        }
        Request::Feed => match feed(live, up_tx) {
            Some((videos, label)) => {
                let _ = up_tx.send(Update::Feed(videos));
                let _ = up_tx.send(Update::Status { live: true, label });
            }
            None => {
                let _ = up_tx.send(Update::Unreached(Unreached::now(
                    false,
                    "YouTube returned no popular videos".into(),
                )));
            }
        },
        Request::PlaylistPage(id) => match live.playlist_page(&id) {
            Ok((playlist, videos)) => {
                let _ = up_tx.send(Update::PlaylistPage {
                    playlist: Box::new(playlist),
                    videos,
                });
            }
            Err(e) => note(format!("playlist {id}: {e}")),
        },
        Request::VideoMeta(id) => answer_watch(live, id, up_tx),
        Request::Channel(id) => match live.channel(&id) {
            Ok(BackendResult::Channel { channel, uploads }) => {
                send_live_status(up_tx);
                let _ = up_tx.send(Update::ChannelPage { channel, uploads });
            }
            Ok(_) => {}
            Err(e) => note(format!("channel {id}: {e}")),
        },
        Request::Captions { id, language } => {
            let _ = up_tx.send(live_captions(live, client, id, language));
        }
        Request::PlayVideo { id, quality } => {
            match live.play(&id, crate::util::quality_height(&quality)) {
                Ok(source) => {
                    let _ = up_tx.send(Update::StreamResolved {
                        video: id,
                        url: source.url,
                        has_audio: source.has_audio,
                        quality: source.quality,
                    });
                }
                Err(said) => {
                    let _ = up_tx.send(Update::Unplayable { video: id, said });
                }
            }
        }
    }
}

/// A watch page, its recommendations and its top comments.
#[cfg(feature = "live")]
fn answer_watch(live: &crate::backend::live::RustyPipeBackend, id: String, up_tx: &Updates) {
    let note = |what: String| {
        let _ = up_tx.send(Update::Note(what));
    };
    match live.watch(&id) {
        Ok(watch) => {
            send_live_status(up_tx);
            let _ = up_tx.send(Update::VideoMeta {
                video: Box::new(watch.video),
                related: watch.related,
            });
            match watch.comments {
                Ok(comments) if !comments.is_empty() => {
                    let _ = up_tx.send(Update::Comments {
                        video: id,
                        comments,
                    });
                }
                Ok(_) => {}
                Err(e) => note(format!("comments: {e}")),
            }
        }
        Err(e) => note(format!("{e}")),
    }
}

/// The caption tracks `id` offers, and the lines of `language` when one was
/// chosen.
#[cfg(feature = "live")]
fn live_captions(
    live: &crate::backend::live::RustyPipeBackend,
    client: Option<&tilo_net::Client>,
    id: String,
    language: Option<String>,
) -> Update {
    let (tracks, lines, failure) = match live.caption_tracks(&id) {
        Ok(tracks) => {
            let (lines, failure) =
                ui::rt::block_on(chosen_caption_lines(client, &tracks, language.as_deref()))
                    .unwrap_or_else(|error| {
                        (None, Some(format!("the caption worker stopped: {error}")))
                    });
            (tracks, lines, failure)
        }
        Err(e) => (Vec::new(), None, Some(e.to_string())),
    };
    Update::Captions {
        video: id,
        tracks,
        language,
        lines,
        failure,
    }
}

/// Settle a request in a build whose rustypipe client did not start: each
/// page stops loading and says there is no source.
#[cfg(feature = "live")]
fn answer_unreached(request: Request, up_tx: &Updates) {
    let update = match request {
        Request::Branding(_) => return,
        Request::Search { query } => Update::SearchResults {
            query,
            videos: Vec::new(),
            playlists: Vec::new(),
            channels: Vec::new(),
        },
        Request::Feed => Update::Unreached(Unreached::now(
            false,
            "the YouTube client did not start".into(),
        )),
        Request::PlayVideo { id, .. } => Update::Unplayable {
            video: id,
            said: None,
        },
        Request::Captions { id, language } => Update::Captions {
            video: id,
            tracks: Vec::new(),
            language,
            lines: None,
            failure: Some("the YouTube client did not start".into()),
        },
        Request::PlaylistPage(id) | Request::VideoMeta(id) | Request::Channel(id) => {
            Update::Note(format!("{id}: the YouTube client did not start"))
        }
    };
    let _ = up_tx.send(update);
}

#[cfg(feature = "live")]
fn send_live_status(up_tx: &Updates) {
    let _ = up_tx.send(Update::Status {
        live: true,
        label: "live (YouTube)".into(),
    });
}

/// The Home catalogue and its status label: `YouTube`'s trending list, or,
/// as that browse has been retired (it answers HTTP 400), a "popular right
/// now" feed composed from each broad topic's most-viewed videos of the week.
/// Every video comes from `YouTube`; `None` when nothing answered.
#[cfg(feature = "live")]
fn feed(
    live: &crate::backend::live::RustyPipeBackend,
    up_tx: &Updates,
) -> Option<(Vec<Video>, String)> {
    match live.trending() {
        Ok(BackendResult::Videos(videos)) if !videos.is_empty() => {
            return Some((videos, "live trending".into()));
        }
        Ok(_) => {}
        Err(e) => {
            let _ = up_tx.send(Update::Note(format!("trending: {e}")));
        }
    }
    let mut lists = Vec::new();
    for seed in POPULAR_SEEDS {
        match live.popular_search(seed) {
            Ok(BackendResult::Videos(videos)) if !videos.is_empty() => lists.push(videos),
            Ok(_) => {}
            Err(e) => {
                let _ = up_tx.send(Update::Note(format!("popular seed {seed:?}: {e}")));
            }
        }
    }
    (!lists.is_empty()).then(|| {
        (
            dedup_by_id(crate::recommend::interleave_newest_first(lists)),
            "live popular".into(),
        )
    })
}

/// Broad, evergreen topics, so the union of their popular videos
/// approximates a general popular feed rather than one niche.
const POPULAR_SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];

/// Drop duplicate videos (same id) while preserving first-seen order — the
/// popular seeds overlap, so the union needs a dedup pass.
fn dedup_by_id(videos: Vec<Video>) -> Vec<Video> {
    let mut seen = std::collections::HashSet::new();
    videos
        .into_iter()
        .filter(|v| seen.insert(v.id.clone()))
        .collect()
}

/// The lines of the track in `language`, if one was asked for and offered:
/// `(lines, why-not)`. A video with no captions is not a failure — it simply
/// offers none, and the page says so.
async fn chosen_caption_lines(
    client: Option<&tilo_net::Client>,
    tracks: &[crate::captions::CaptionTrack],
    language: Option<&str>,
) -> (Option<Vec<crate::captions::CaptionCue>>, Option<String>) {
    let (Some(language), Some(client)) = (language, client) else {
        return (None, None);
    };
    let Some(track) = tracks.iter().find(|track| track.language == language) else {
        return (None, None);
    };
    match fetch_timed_text(client, &track.url).await {
        Ok(lines) if !lines.is_empty() => (Some(lines), None),
        Ok(_) => (None, Some("the caption track was empty".into())),
        Err(error) => (None, Some(format!("{error:#}"))),
    }
}

/// A caption track's `WebVTT` text.
async fn fetch_timed_text(
    client: &tilo_net::Client,
    url: &str,
) -> anyhow::Result<Vec<crate::captions::CaptionCue>> {
    let text = client
        .get(url)
        .timeout(CAPTION_TIMEOUT)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(crate::captions::parse_vtt(&text))
}

/// How long the timed text of a caption track may take.
const CAPTION_TIMEOUT: Duration = Duration::from_secs(8);

/// The worker where rustypipe cannot run (a browser page): every request
/// answered from `YouTube`'s own web API ([`crate::innertube`]) through
/// `tilo_net` (the host's fetch proxy where a site refuses cross-origin
/// reads). Updates are offered, never waited on: the page has one thread and
/// the UI drains them.
#[cfg(not(feature = "live"))]
async fn task_worker(requests: Receiver<Request>, updates: Updates) {
    let post = |update: Update| {
        let _ = updates.try_send(update);
    };
    let Some(client) = client() else {
        post(Update::Unreached(Unreached::Failed(
            "the network client did not start".into(),
        )));
        return;
    };
    let youtube = crate::innertube::InnerTube::new(client.clone());
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
        answer(&client, &youtube, request, &updates).await;
    }
}

/// Why `YouTube`'s web API gave no answer, as the page should say it: a
/// refusal the browser hid (no CORS headers) is unreadable without a relay.
#[cfg(not(feature = "live"))]
fn unreached(error: &anyhow::Error) -> Unreached {
    let hidden = error
        .downcast_ref::<tilo_net::NetError>()
        .is_some_and(tilo_net::NetError::may_be_cors);
    Unreached::now(hidden, format!("{error:#}"))
}

/// Answer one request in the page worker from `YouTube`'s web API.
#[cfg(not(feature = "live"))]
async fn answer(
    client: &tilo_net::Client,
    youtube: &crate::innertube::InnerTube,
    request: Request,
    updates: &Updates,
) {
    let post = |update: Update| {
        let _ = updates.try_send(update);
    };
    match request {
        Request::Branding(id) => branding(client.clone(), id, updates.clone()).await,
        Request::Search { query } => match youtube.search(&query).await {
            Ok(found) => {
                post(youtube_status());
                post(Update::SearchResults {
                    query,
                    videos: found.videos,
                    playlists: Vec::new(),
                    channels: found.channels,
                });
            }
            Err(error) => post(Update::Unreached(unreached(&error))),
        },
        Request::Feed => match youtube_popular(youtube).await {
            Ok(videos) if !videos.is_empty() => {
                post(Update::Feed(videos));
                post(youtube_status());
            }
            Ok(_) => post(Update::Unreached(Unreached::now(
                false,
                "the popular list is empty".into(),
            ))),
            Err(error) => post(Update::Unreached(unreached(&error))),
        },
        Request::VideoMeta(id) => {
            if let Ok((video, related)) = youtube.video(&id).await {
                post(Update::VideoMeta {
                    video: Box::new(video),
                    related,
                });
            }
        }
        Request::Channel(id) => {
            if let Ok((channel, uploads)) = youtube.channel(&id).await {
                post(Update::ChannelPage {
                    channel: Box::new(channel),
                    uploads,
                });
            }
        }
        Request::PlaylistPage(id) => {
            post(Update::Note(format!("playlist {id}: unavailable here")));
        }
        Request::Captions { id, language } => {
            let tracks = youtube.caption_tracks(&id).await.unwrap_or_default();
            let (lines, failure) =
                chosen_caption_lines(Some(client), &tracks, language.as_deref()).await;
            post(Update::Captions {
                video: id,
                tracks,
                language,
                lines,
                failure,
            });
        }
        Request::PlayVideo { id, quality } => {
            let wanted = crate::util::quality_height(&quality);
            post(match youtube.stream(&id, wanted).await {
                Ok(stream) => Update::StreamResolved {
                    video: id,
                    url: stream.url,
                    has_audio: true,
                    quality: stream.quality,
                },
                Err(error) => Update::Unplayable {
                    video: id,
                    said: crate::innertube::refusal(&error),
                },
            });
        }
    }
}

/// The popular feed from `YouTube`: each topic's most-viewed videos of the
/// week, interleaved; the last failure when no topic answered.
#[cfg(not(feature = "live"))]
async fn youtube_popular(youtube: &crate::innertube::InnerTube) -> anyhow::Result<Vec<Video>> {
    let mut lists = Vec::new();
    let mut last = None;
    for seed in POPULAR_SEEDS {
        match youtube.popular(seed).await {
            Ok(videos) if !videos.is_empty() => lists.push(videos),
            Ok(_) => {}
            Err(error) => last = Some(error),
        }
    }
    match last {
        Some(error) if lists.is_empty() => Err(error),
        _ => Ok(dedup_by_id(crate::recommend::interleave_newest_first(
            lists,
        ))),
    }
}

#[cfg(not(feature = "live"))]
fn youtube_status() -> Update {
    Update::Status {
        live: true,
        label: "live (YouTube)".into(),
    }
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
        {
            let j = category_json();
            assert!(j.contains("sponsor"));
            assert!(j.contains("music_offtopic"));
        }
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
        {
            assert!(parse_skip_segments(&serde_json::json!({})).is_empty());
        }
    }

    #[test]
    fn api_workers_queue_and_query_branding_endpoints() {
        {
            let (tx, _hold_queue) = std::sync::mpsc::sync_channel(1);
            let (_unused_tx, rx) = std::sync::mpsc::sync_channel(1);
            let worker = Worker {
                tx,
                rx,
                suggest: None,
            };
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
                (
                    "/votes?videoId=fixture",
                    200,
                    r#"{"likes":900,"dislikes":42}"#,
                ),
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
                block(fetch_votes_at(&c, &format!("{base}/votes"), "fixture"))
                    .expect("votes response"),
                Votes {
                    dislikes: 42,
                    likes: Some(900),
                }
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

    #[test]
    fn sources_that_fail_while_online_are_not_offline() {
        // Case `only_a_device_without_network_is_offline`. In a page the
        // browser hides why a cross-origin read failed; with no fetch proxy
        // that is the answer to give, never "offline" while online.
        {
            let classify = |online, relay, unreadable| {
                Unreached::classify(online, relay, unreadable, "HTTP 503".into())
            };
            assert_eq!(classify(false, false, true), Unreached::Offline);
            assert_eq!(classify(false, true, false), Unreached::Offline);
            assert_eq!(classify(true, false, true), Unreached::NoRelay);
            let failed = Unreached::Failed("HTTP 503".into());
            assert_eq!(classify(true, true, true), failed, "a relay was tried");
            assert_eq!(classify(true, false, false), failed);
        }
    }
}
