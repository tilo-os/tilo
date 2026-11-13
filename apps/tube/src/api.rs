//! Live data sources: `YouTube` through rustypipe ([`crate::backend::live`]) where it runs, its own
//! web API ([`crate::innertube`]) in a browser page, where rustypipe cannot; `SponsorBlock`
//! (segments), `DeArrow` (de-clickbait titles/thumbnails) and Return `YouTube` Dislike beside it.

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

#[derive(Clone, Debug)]
pub enum Request {
    Branding(String),
    Search {
        query: String,
    },
    Feed,
    VideoMeta(String),
    Channel(String),
    PlaylistPage(String),
    PlayVideo {
        id: String,
        quality: String,
    },
    Captions {
        id: String,
        language: Option<String>,
    },
}

#[derive(Clone)]
pub struct Updates {
    tx: SyncSender<Update>,
    wake: ui::WakeHandle,
}

impl Updates {
    pub fn send(&self, update: Update) -> Result<(), std::sync::mpsc::SendError<Update>> {
        self.tx.send(update)?;
        self.wake.wake();
        Ok(())
    }

    pub fn try_send(
        &self,
        update: Update,
    ) -> Result<(), Box<std::sync::mpsc::TrySendError<Update>>> {
        self.tx.try_send(update).map_err(Box::new)?;
        self.wake.wake();
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub enum Update {
    Sponsor(String, Vec<Segment>),
    DeArrow {
        video: String,
        title: Option<String>,
        thumbnail_url: Option<String>,
    },
    Dislikes {
        video: String,
        count: u64,
        likes: Option<u64>,
    },
    SearchResults {
        query: String,
        videos: Vec<Video>,
        playlists: Vec<Playlist>,
        channels: Vec<Channel>,
    },
    PlaylistPage {
        playlist: Box<Playlist>,
        videos: Vec<Video>,
    },
    Feed(Vec<Video>),
    VideoMeta {
        video: Box<Video>,
        related: Vec<Video>,
    },
    Comments {
        video: String,
        comments: Vec<Comment>,
    },
    ChannelPage {
        channel: Box<Channel>,
        uploads: Vec<Video>,
    },
    Status {
        live: bool,
        label: String,
    },
    Unreached(Unreached),
    StreamResolved {
        video: String,
        url: String,
        has_audio: bool,
        quality: String,
    },
    Captions {
        video: String,
        tracks: Vec<crate::captions::CaptionTrack>,
        language: Option<String>,
        lines: Option<Vec<crate::captions::CaptionCue>>,
        failure: Option<String>,
    },
    Unplayable {
        video: String,
        said: Option<String>,
    },
    Suggestions {
        query: String,
        videos: Vec<Video>,
    },
    Note(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unreached {
    Offline,
    NoRelay,
    Failed(String),
}

impl Unreached {
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

    fn now(unreadable: bool, last: String) -> Self {
        Self::classify(
            tilo_net::status::online(),
            tilo_net::proxy::endpoint().is_some(),
            unreadable,
            last,
        )
    }
}

fn client() -> Option<tilo_net::Client> {
    tilo_net::Client::builder()
        .user_agent("tube/0.1 (+tilo)")
        .timeout(Duration::from_secs(8))
        .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
        .build()
        .ok()
}

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

pub fn parse_dearrow(resp: &serde_json::Value, video_id: &str) -> (Option<String>, Option<String>) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Votes {
    pub dislikes: u64,
    pub likes: Option<u64>,
}

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

pub fn parse_votes(resp: &serde_json::Value) -> Option<Votes> {
    Some(Votes {
        dislikes: resp.get("dislikes")?.as_u64()?,
        likes: resp.get("likes").and_then(serde_json::Value::as_u64),
    })
}

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

pub struct Worker {
    tx: SyncSender<Request>,
    rx: Receiver<Update>,
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
            if let Err(e) = ui::rt::spawn_thread("tube-fetch", move || live_worker(&req_rx, &up_tx))
            {
                tracing::error!("failed to spawn the tube-fetch worker thread: {e}");
            }
        }
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

    pub fn request(&self, req: Request) -> Result<(), std::sync::mpsc::TrySendError<Request>> {
        self.tx.try_send(req)
    }

    pub fn suggest(&self, query: &str) -> bool {
        let Some((suggesting, updates)) = &self.suggest else {
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

    pub fn poll(&self) -> Vec<Update> {
        self.rx.try_iter().take(MAX_UPDATES_PER_POLL).collect()
    }

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

/// Broad, evergreen topics, so the union of their popular videos approximates a general popular
/// feed rather than one niche.
const POPULAR_SEEDS: [&str; 6] = ["news", "music", "gaming", "technology", "sports", "science"];

fn dedup_by_id(videos: Vec<Video>) -> Vec<Video> {
    let mut seen = std::collections::HashSet::new();
    videos
        .into_iter()
        .filter(|v| seen.insert(v.id.clone()))
        .collect()
}

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

#[cfg(not(feature = "live"))]
fn unreached(error: &anyhow::Error) -> Unreached {
    let hidden = error
        .downcast_ref::<tilo_net::NetError>()
        .is_some_and(tilo_net::NetError::may_be_cors);
    Unreached::now(hidden, format!("{error:#}"))
}

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
