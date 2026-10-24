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
//! ## API surface used (rustypipe 0.11.x — verified)
//! * construct: `RustyPipe::builder()…build()?`, then `rp.query()`
//! * `query.search::<YouTubeItem, _>(q).await` → `SearchResult { items, .. }`
//! * `query.video_details(id).await` → `VideoDetails` (has `.recommended`,
//!   so per-video **related** is a field, not its own endpoint)
//! * `query.channel_videos(id).await` → channel + uploads
//! * `query.trending().await` → `Vec<VideoItem>`
//! * `query.player(id).await` → `VideoPlayer` (stream URLs) — gated behind a
//!   `PoToken`
//!
//! The client is built with `no_botguard`: metadata needs no `PoToken`, and
//! playback resolves through an Invidious proxy ([`crate::backend::proxy`]),
//! so no external `BotGuard` binary is ever needed. rustypipe embeds
//! `rquickjs`; no ffmpeg / yt-dlp.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{
    ChannelItem, Comment as RpComment, PlaylistItem, Verification, VideoItem, YouTubeItem,
};
use rustypipe::param::search_filter::{ItemType, Order, SearchFilter, UploadDate};
use tilo_innertube as innertube;

use crate::backend::{BackendError, BackendOutcome, BackendResult, YtBackend};
use crate::model::{Channel, Comment, Playlist, Video};

/// A rustypipe search split into Tube's three result kinds. Used to enrich the
/// primary (yt-dlp) search — which returns videos only, without creator avatars
/// — with playlists, creator rows, and the channel avatars carried on every
/// rustypipe item.
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
    rp: RustyPipe,
    rt: &'static ui::rt::AppRuntime,
}

impl RustyPipeBackend {
    pub fn new() -> Result<Self, BackendError> {
        // rustypipe `tokio::spawn`s background work (the visitor-data
        // prefetch and the connection tasks behind its HTTP client) that must
        // keep running after the query that started it returns. On the app
        // runtime those tasks live on the runtime thread and progress
        // independently of this worker, which only polls its own query.
        let rt = ui::rt::try_app().map_err(|e| BackendError(format!("tokio runtime: {e}")))?;
        // rustypipe's reqwest client takes the process-default TLS provider.
        tilo_net::install_crypto_provider();
        // Build the client *inside* the runtime so anything rustypipe wires up
        // at construction (the reqwest pool, cache tasks) has a reactor.
        let rp = rt
            .block_on(async {
                // `no_reporter()`: rustypipe's default reporter writes a
                // `rustypipe_reports/*.json` file into the process cwd on every
                // parser warning. A shipped app must not litter the working
                // directory, so disable it (warnings still degrade gracefully —
                // core fields are extracted regardless).
                RustyPipe::builder().no_reporter().no_botguard().build()
            })
            .map_err(|e| BackendError(format!("tokio runtime: {e}")))?
            .map_err(|e| BackendError(format!("rustypipe build: {e}")))?;
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
    /// rows — the enrichment source for the primary backend, which returns only
    /// videos (and no avatars). Real `InnerTube` data in a single call.
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
        let q = self.rp.query();
        let d = self
            .block(innertube::video_details(&q, id))
            .map_err(|e| BackendError(format!("video: {e}")))?;
        let channel_id = d.channel.id.clone();
        let v = Video {
            id: d.id.clone(),
            title: d.name,
            dearrow_title: None,
            channel_id: channel_id.clone(),
            channel_avatar_url: best_thumb(&d.channel.avatar),
            channel_name: d.channel.name,
            channel_seed: channel_id,
            views: d.view_count,
            published: d.publish_date_txt.unwrap_or_default(),
            // Duration comes from the player/streams response; left 0 here and
            // refined when streams are resolved (behind a PoToken).
            duration: 0,
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
        Ok(BackendResult::Video(Box::new(v)))
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
        // Related ("up next") is a field on the video detail response, not a
        // dedicated endpoint — fetch details and lift `.recommended`.
        let q = self.rp.query();
        let d = self
            .block(innertube::video_details(&q, id))
            .map_err(|e| BackendError(format!("related: {e}")))?;
        let vids = d
            .recommended
            .items
            .into_iter()
            .map(map_video_item)
            .collect();
        Ok(BackendResult::Videos(vids))
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
        // `video_details` carries an (initially empty) top-comments paginator
        // holding the continuation token; fetching its first page pulls the real
        // "most liked first" comments via the `next`/comments endpoint.
        let q = self.rp.query();
        let d = self
            .block(innertube::video_details(&q, id))
            .map_err(|e| BackendError(format!("comments details {id}: {e}")))?;
        let page = self
            .block(innertube::next_comments(&d.top_comments, &q))
            .map_err(|e| BackendError(format!("comments page {id}: {e}")))?;
        let items = page.map(|p| p.items).unwrap_or_default();
        Ok(items.into_iter().take(60).map(map_comment).collect())
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
