//! Live backend — real `YouTube` data via the pure-Rust `rustypipe` `InnerTube`
//! client. **Compiled only under `--features live`.**
//!
//! `rustypipe` is async (tokio); the [`YtBackend`] trait is sync because it is
//! driven from Tube's blocking fetch worker. This backend therefore waits on
//! each query with the shared app runtime's `block_on`: the query future is
//! polled on the worker thread while the runtime thread drives sockets,
//! timers and whatever rustypipe spawns. That keeps the async dependency
//! fully contained here — no other module learns about tokio.
//!
//! ## API surface used (rustypipe 0.11.x, through [`tilo_innertube`])
//! * `search` → videos, playlists and creator rows of a query
//! * `video_details` → a watch page: metadata, recommendations ("up next") and
//!   the first page of comments
//! * `player` → the stream to play ([`tilo_innertube::whole_file_stream`]),
//!   the duration and the caption tracks
//! * `channel_videos`, `playlist`, `trending`, `search_filter` (the popular
//!   feed)
//!
//! The client is built with `no_botguard`: metadata needs no `PoToken`, and
//! the player client [`tilo_innertube::PLAYER_CLIENT`] needs none either.
//! rustypipe embeds `rquickjs`; no ffmpeg, no yt-dlp.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{
    ChannelItem, Comment as RpComment, PlaylistItem, Verification, VideoItem, YouTubeItem,
};
use rustypipe::param::search_filter::{ItemType, Order, SearchFilter, UploadDate};
use tilo_innertube as innertube;

use crate::backend::{BackendError, BackendOutcome, BackendResult, PlaybackSource, YtBackend};
use crate::model::{Channel, Comment, Playlist, Video};

/// A rustypipe search split into Tube's three result kinds.
pub struct SearchItems {
    pub videos: Vec<Video>,
    pub playlists: Vec<Playlist>,
    pub channels: Vec<Channel>,
}

/// rustypipe marks channels with a `Verification` enum; map "has a badge" onto
/// Tube's boolean `verified`.
fn is_verified(v: Verification) -> bool {
    !matches!(v, Verification::None)
}

/// The live rustypipe-backed data source.
pub struct RustyPipeBackend {
    rp: &'static RustyPipe,
    rt: &'static ui::rt::AppRuntime,
}

/// The one rustypipe client of the process, shared by the fetch worker and
/// the search suggestions. It must be first built inside the app runtime, so
/// that what rustypipe wires up at construction (the reqwest pool, cache
/// tasks) has a reactor.
fn shared_client() -> Result<&'static RustyPipe, BackendError> {
    static CLIENT: std::sync::OnceLock<Result<RustyPipe, String>> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // rustypipe's reqwest client takes the process-default TLS provider.
            tilo_net::install_crypto_provider();
            // `no_reporter()`: rustypipe's default reporter writes a
            // `rustypipe_reports/*.json` file into the process cwd on every
            // parser warning. A shipped app must not litter the working
            // directory, so disable it (warnings still degrade gracefully —
            // core fields are extracted regardless).
            RustyPipe::builder()
                .no_reporter()
                .no_botguard()
                .build()
                .map_err(|e| format!("rustypipe build: {e}"))
        })
        .as_ref()
        .map_err(|e| BackendError(e.clone()))
}

/// The videos `YouTube` finds for `query`, asked on the app runtime (the
/// platform search's suggestions); empty when it does not answer.
#[allow(
    clippy::future_not_send,
    reason = "tilo-innertube's queries are boxed without Send so their code stays \
              in the engine library; the caller polls this on one pinned thread"
)]
pub async fn suggested_videos(query: &str) -> Vec<Video> {
    let Ok(rp) = shared_client() else {
        return Vec::new();
    };
    let q = rp.query();
    innertube::search(&q, query)
        .await
        .map(|res| videos_from_items(res.items.items))
        .unwrap_or_default()
}

/// A watch page: the video, what `YouTube` recommends next, and its top
/// comments (empty when they could not be read).
pub struct Watch {
    pub video: Video,
    pub related: Vec<Video>,
    pub comments: Result<Vec<Comment>, BackendError>,
}

impl RustyPipeBackend {
    pub fn new() -> Result<Self, BackendError> {
        // rustypipe `tokio::spawn`s background work (the visitor-data
        // prefetch and the connection tasks behind its HTTP client) that must
        // keep running after the query that started it returns. On the app
        // runtime those tasks live on the runtime thread and progress
        // independently of this worker, which only polls its own query.
        let rt = ui::rt::try_app().map_err(|e| BackendError(format!("tokio runtime: {e}")))?;
        let rp = rt
            .block_on(async { shared_client() })
            .map_err(|e| BackendError(format!("tokio runtime: {e}")))??;
        Ok(Self { rp, rt })
    }

    /// Wait on one rustypipe query from the calling worker thread. A query
    /// error and a refused wait (e.g. called from inside a runtime) both come
    /// back as the error's message.
    fn block<F, T, E>(&self, fut: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        match self.rt.block_on(fut) {
            Ok(result) => result.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Run one search and split the results into videos, playlists and creator
    /// rows, all from one `InnerTube` call.
    pub fn search_items(&self, query: &str) -> Result<SearchItems, BackendError> {
        let q = self.rp.query();
        let res = self
            .block(innertube::search(&q, query))
            .map_err(|e| BackendError(format!("search_items: {e}")))?;
        let mut out = SearchItems {
            videos: Vec::new(),
            playlists: Vec::new(),
            channels: Vec::new(),
        };
        for it in res.items.items {
            match it {
                YouTubeItem::Video(v) => out.videos.push(map_video_item(v)),
                YouTubeItem::Playlist(p) => out.playlists.push(map_playlist_item(p)),
                YouTubeItem::Channel(c) => out.channels.push(map_channel_item(c)),
            }
        }
        Ok(out)
    }

    /// Fetch a playlist page: its metadata + ordered video list.
    pub fn playlist_page(&self, id: &str) -> Result<(Playlist, Vec<Video>), BackendError> {
        let q = self.rp.query();
        let pl = self
            .block(innertube::playlist(&q, id))
            .map_err(|e| BackendError(format!("playlist {id}: {e}")))?;
        let videos: Vec<Video> = pl.videos.items.into_iter().map(map_video_item).collect();
        let (channel_id, channel_name) = pl
            .channel
            .map_or_else(|| (String::new(), "Unknown".into()), |c| (c.id, c.name));
        let thumb = best_thumb(&pl.thumbnail);
        let thumbnail_url = if thumb.is_empty() {
            videos
                .first()
                .map(|v| v.thumbnail_url.clone())
                .unwrap_or_default()
        } else {
            thumb
        };
        let playlist = Playlist {
            id: pl.id.clone(),
            title: pl.name,
            channel_id,
            channel_name,
            thumbnail_url,
            seed: pl.id,
            video_count: pl.video_count,
            video_ids: videos.iter().map(|v| v.id.clone()).collect(),
        };
        Ok((playlist, videos))
    }

    /// A watch page from one `video_details` call, with the duration from
    /// the player answer (the details carry none).
    pub fn watch(&self, id: &str) -> Result<Watch, BackendError> {
        let q = self.rp.query();
        let d = self
            .block(innertube::video_details(&q, id))
            .map_err(|e| BackendError(format!("video {id}: {e}")))?;
        let duration = self
            .block(innertube::player(&q, id))
            .map_or(0, |player| player.details.duration);
        let comments = self
            .block(innertube::next_comments(&d.top_comments, &q))
            .map(|page| {
                page.map(|p| p.items)
                    .unwrap_or_default()
                    .into_iter()
                    .take(60)
                    .map(map_comment)
                    .collect()
            })
            .map_err(|e| BackendError(format!("comments {id}: {e}")));
        let channel_id = d.channel.id.clone();
        let video = Video {
            id: d.id.clone(),
            title: d.name,
            dearrow_title: None,
            channel_id: channel_id.clone(),
            channel_avatar_url: best_thumb(&d.channel.avatar),
            channel_name: d.channel.name,
            channel_seed: channel_id,
            views: d.view_count,
            published: d.publish_date_txt.unwrap_or_default(),
            duration,
            // `description` is rustypipe's `RichText`; flatten to plain text.
            description: d.description.to_plaintext(),
            likes: u64::from(d.like_count.unwrap_or(0)),
            thumbnail_url: crate::backend::yt_thumb_url(&d.id),
            seed: d.id,
            category: "Video".into(),
            dearrow_thumb: false,
            sponsor: Vec::new(),
            comments: Vec::new(),
            live: d.is_live,
        };
        let related = d
            .recommended
            .items
            .into_iter()
            .map(map_video_item)
            .collect();
        Ok(Watch {
            video,
            related,
            comments,
        })
    }

    /// The stream to play for `id`, no taller than `max_height` when there is
    /// one that small. The error is `YouTube`'s own words when it refused to
    /// play the video (such as a bot check), else `None`.
    pub fn play(
        &self,
        id: &str,
        max_height: Option<u32>,
    ) -> Result<PlaybackSource, Option<String>> {
        let q = self.rp.query();
        let player = self
            .rt
            .block_on(innertube::player(&q, id))
            .map_err(|_| None)?;
        let player = player.map_err(|error| match error {
            rustypipe::error::Error::Extraction(
                rustypipe::error::ExtractionError::Unavailable { msg, .. },
            ) if !msg.is_empty() => Some(msg),
            _ => None,
        })?;
        let stream = innertube::whole_file_stream(&player.video_streams, max_height).ok_or(None)?;
        Ok(PlaybackSource {
            url: stream.url.clone(),
            has_audio: true,
            quality: stream.quality.clone(),
        })
    }

    /// The caption tracks `id` offers. Their timed text is asked for as
    /// `WebVTT`, which [`crate::captions::parse_vtt`] reads.
    pub fn caption_tracks(
        &self,
        id: &str,
    ) -> Result<Vec<crate::captions::CaptionTrack>, BackendError> {
        let q = self.rp.query();
        let player = self
            .block(innertube::player(&q, id))
            .map_err(|e| BackendError(format!("captions {id}: {e}")))?;
        Ok(player
            .subtitles
            .into_iter()
            .map(|subtitle| crate::captions::CaptionTrack {
                label: subtitle.lang_name,
                automatic: subtitle.auto_generated,
                url: as_vtt(&subtitle.url),
                language: subtitle.lang,
            })
            .collect())
    }
}

/// A timed-text URL asking for `WebVTT`: rustypipe names its own format
/// (`fmt=srv3`), and of a repeated parameter `YouTube` reads the first.
fn as_vtt(url: &str) -> String {
    let (base, query) = url.split_once('?').unwrap_or((url, ""));
    let mut params: Vec<&str> = query
        .split('&')
        .filter(|param| !param.is_empty() && !param.starts_with("fmt="))
        .collect();
    params.push("fmt=vtt");
    format!("{base}?{}", params.join("&"))
}

/// Map a rustypipe [`VideoItem`] onto Tube's [`Video`]. Fields rustypipe doesn't
/// carry on a list item (description, likes, comments, sponsor) default empty
/// and are filled in by a later `video_details` / `SponsorBlock` fetch.
fn map_video_item(it: VideoItem) -> Video {
    let channel = it.channel;
    let (channel_id, channel_name, channel_avatar_url) = channel.map_or_else(
        || (String::new(), "Unknown".into(), String::new()),
        |c| (c.id, c.name, best_thumb(&c.avatar)),
    );
    Video {
        id: it.id.clone(),
        title: it.name,
        dearrow_title: None,
        channel_id: channel_id.clone(),
        channel_name,
        channel_seed: channel_id,
        channel_avatar_url,
        views: it.view_count.unwrap_or(0),
        published: it.publish_date_txt.unwrap_or_default(),
        duration: it.duration.unwrap_or(0),
        description: it.short_description.unwrap_or_default(),
        likes: 0,
        thumbnail_url: crate::backend::yt_thumb_url(&it.id),
        seed: it.id,
        category: "Search".into(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live: it.is_live,
    }
}

/// Pick the highest-resolution thumbnail URL from a rustypipe thumbnail set
/// (largest width×height), or the empty string when none are present. Used for
/// channel avatars carried on feed/search items so the real creator icon loads.
///
/// The URL is normalized to an absolute `https:` one: rustypipe hands back
/// **protocol-relative** URLs (`//yt3.ggpht.com/…`) for some items — notably
/// creator rows — which the reqwest image transport cannot fetch (no scheme),
/// so the card silently fell back to initials. Normalizing here fixes every
/// consumer (`best_thumb` is the single extraction chokepoint).
fn best_thumb(thumbs: &[rustypipe::model::Thumbnail]) -> String {
    thumbs
        .iter()
        .max_by_key(|t| u64::from(t.width) * u64::from(t.height))
        .map(|t| absolute_url(&t.url))
        .unwrap_or_default()
}

/// Promote a protocol-relative URL (`//host/path`) to `https://host/path`.
/// Absolute and empty URLs pass through unchanged.
fn absolute_url(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

/// First one/two initials of an author name, for the comment avatar chip.
fn initials(name: &str) -> String {
    let mut out = String::new();
    for word in name.split_whitespace().take(2) {
        if let Some(c) = word.chars().next() {
            out.push(c.to_ascii_uppercase());
        }
    }
    if out.is_empty() {
        out.push('?');
    }
    out
}

/// Map a rustypipe [`PlaylistItem`] (a search-result stub) onto Tube's
/// [`Playlist`]. The video list is empty until the playlist page is opened.
fn map_playlist_item(p: PlaylistItem) -> Playlist {
    let (channel_id, channel_name) = p
        .channel
        .map_or_else(|| (String::new(), "Unknown".into()), |c| (c.id, c.name));
    Playlist {
        id: p.id.clone(),
        title: p.name,
        channel_id,
        channel_name,
        thumbnail_url: best_thumb(&p.thumbnail),
        seed: p.id,
        video_count: p.video_count.unwrap_or(0),
        video_ids: Vec::new(),
    }
}

/// Map a rustypipe [`ChannelItem`] (a search-result creator) onto Tube's
/// [`Channel`], carrying the real avatar so the creator row shows a photo.
fn map_channel_item(c: ChannelItem) -> Channel {
    Channel {
        id: c.id.clone(),
        name: c.name,
        handle: c.handle.unwrap_or_else(|| c.id.clone()),
        subscribers: c.subscriber_count.unwrap_or(0),
        verified: is_verified(c.verification),
        description: c.short_description,
        seed: c.id,
        avatar_url: best_thumb(&c.avatar),
        banner_url: String::new(),
    }
}

/// Map a rustypipe [`RpComment`] onto Tube's plain [`Comment`].
fn map_comment(c: RpComment) -> Comment {
    let author = c.author.map_or_else(|| "Unknown".into(), |a| a.name);
    let initials = initials(&author);
    Comment {
        author,
        initials,
        text: c.text.to_plaintext(),
        likes: u64::from(c.like_count.unwrap_or(0)),
        time_ago: c.publish_date_txt,
        pinned: c.pinned,
        hearted: c.hearted,
        replies: c.reply_count,
    }
}

/// Pull the `Video` rows out of a rustypipe search/trending/recommended list,
/// keeping only the `Video` variants.
fn videos_from_items(items: impl IntoIterator<Item = YouTubeItem>) -> Vec<Video> {
    items
        .into_iter()
        .filter_map(|i| match i {
            YouTubeItem::Video(v) => Some(map_video_item(v)),
            _ => None,
        })
        .collect()
}

impl YtBackend for RustyPipeBackend {
    fn name(&self) -> &'static str {
        "youtube (live)"
    }

    fn search(&self, query: &str) -> BackendOutcome {
        let q = self.rp.query();
        let res = self
            .block(innertube::search(&q, query))
            .map_err(|e| BackendError(format!("search: {e}")))?;
        Ok(BackendResult::Videos(videos_from_items(res.items.items)))
    }

    fn video(&self, id: &str) -> BackendOutcome {
        Ok(BackendResult::Video(Box::new(self.watch(id)?.video)))
    }

    fn channel(&self, id: &str) -> BackendOutcome {
        let q = self.rp.query();
        let ch = self
            .block(innertube::channel_videos(&q, id))
            .map_err(|e| BackendError(format!("channel: {e}")))?;
        let channel = Channel {
            id: ch.id.clone(),
            name: ch.name.clone(),
            handle: ch.handle.clone().unwrap_or_else(|| ch.id.clone()),
            subscribers: ch.subscriber_count.unwrap_or(0),
            verified: is_verified(ch.verification),
            description: ch.description.clone(),
            seed: ch.id.clone(),
            // Real channel avatar from the channel response; the banner isn't in
            // this payload, so the seed gradient still backs the header.
            avatar_url: best_thumb(&ch.avatar),
            banner_url: String::new(),
        };
        let uploads = ch.content.items.into_iter().map(map_video_item).collect();
        Ok(BackendResult::Channel {
            channel: Box::new(channel),
            uploads,
        })
    }

    fn related(&self, id: &str) -> BackendOutcome {
        Ok(BackendResult::Videos(self.watch(id)?.related))
    }

    fn trending(&self) -> BackendOutcome {
        let q = self.rp.query();
        let items = self
            .block(innertube::trending(&q))
            .map_err(|e| BackendError(format!("trending: {e}")))?;
        Ok(BackendResult::Videos(
            items.into_iter().map(map_video_item).collect(),
        ))
    }

    fn comments(&self, id: &str) -> Result<Vec<Comment>, BackendError> {
        self.watch(id)?.comments
    }

    fn playlist(&self, id: &str) -> Result<(Playlist, Vec<Video>), BackendError> {
        self.playlist_page(id)
    }

    fn resolve_playback_source(
        &self,
        id: &str,
        max_height: Option<u32>,
    ) -> Result<PlaybackSource, BackendError> {
        self.play(id, max_height).map_err(|said| {
            BackendError(said.unwrap_or_else(|| format!("no playable stream for {id}")))
        })
    }

    fn popular_search(&self, query: &str) -> BackendOutcome {
        // "Popular right now" as a real, no-login-required query: view-count
        // sorted, uploaded within the last week, videos only. This is genuine
        // YouTube data (unlike the dead `FEtrending` browse) and is what the
        // composed home feed leans on.
        let q = self.rp.query();
        let filter = SearchFilter::new()
            .item_type(ItemType::Video)
            .sort(Order::Views)
            .date(UploadDate::Week);
        let res = self
            .block(innertube::search_filter(&q, query, &filter))
            .map_err(|e| BackendError(format!("popular_search {query:?}: {e}")))?;
        Ok(BackendResult::Videos(videos_from_items(res.items.items)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caption_urls_ask_for_webvtt_once() {
        assert_eq!(
            as_vtt("https://www.youtube.com/api/timedtext?v=x&lang=en&fmt=srv3"),
            "https://www.youtube.com/api/timedtext?v=x&lang=en&fmt=vtt"
        );
        assert_eq!(
            as_vtt("https://www.youtube.com/api/timedtext?v=x"),
            "https://www.youtube.com/api/timedtext?v=x&fmt=vtt"
        );
    }

    #[test]
    fn absolute_url_promotes_protocol_relative_avatars() {
        // rustypipe hands back protocol-relative creator-row avatar URLs; reqwest
        // can't fetch those, so they must be promoted to https.
        assert_eq!(
            absolute_url("//yt3.ggpht.com/abc=s176"),
            "https://yt3.ggpht.com/abc=s176",
        );
        assert_eq!(
            absolute_url("https://yt3.ggpht.com/x"),
            "https://yt3.ggpht.com/x"
        );
        assert_eq!(absolute_url("http://example.com/y"), "http://example.com/y");
        assert_eq!(absolute_url(""), "");
    }
}
