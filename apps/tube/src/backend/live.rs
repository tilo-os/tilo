//! Live backend — real YouTube data via the pure-Rust `rustypipe` InnerTube
//! client. **Compiled only under `--features live`.**
//!
//! `rustypipe` is async (tokio); the [`YtBackend`] trait is sync because it is
//! driven from Tube's blocking fetch worker. This backend therefore owns a
//! small current-thread tokio runtime and `block_on`s each query. That keeps
//! the async dependency fully contained here — no other module learns about
//! tokio.
//!
//! ## API surface used (rustypipe 0.11.x — verified)
//! * construct: `RustyPipe::builder()…build()?`, then `rp.query()`
//! * `query.search::<YouTubeItem, _>(q).await` → `SearchResult { items, .. }`
//! * `query.video_details(id).await` → `VideoDetails` (has `.recommended`,
//!   so per-video **related** is a field, not its own endpoint)
//! * `query.channel_videos(id).await` → channel + uploads
//! * `query.trending().await` → `Vec<VideoItem>`
//! * `query.player(id).await` → `VideoPlayer` (stream URLs) — gated behind a
//!   PoToken
//!
//! ## PoToken
//! rustypipe has **no `PoTokenProvider` trait**; it drives PoTokens through an
//! external `rustypipe-botguard` *binary* (`RustyPipeBuilder::botguard_bin` /
//! `no_botguard` / `botguard_snapshot_file` / `po_token_cache`) or you mint
//! in-process with the `rustypipe-botguard` library's `Botguard::mint_token`.
//! Tube's own [`crate::backend::potoken::PoTokenProvider`] is the *Tube-side*
//! swappable seam; [`bind_potoken`] documents how a provider choice maps onto
//! rustypipe's knobs. The default ([`NullPoToken`]) selects `.no_botguard()`,
//! giving metadata-only browsing with zero extra runtime deps.
//!
//! Pure Rust: rustypipe embeds `rquickjs`; no ffmpeg / yt-dlp. The botguard
//! binary (Deno+JSDOM) is a *runtime* dep only when real streams are needed.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::richtext::ToPlaintext;
use rustypipe::model::{
    Verification, VideoCodec, VideoFormat, VideoItem, VideoPlayer as RpVideoPlayer, VideoStream,
    YouTubeItem,
};
use rustypipe::param::search_filter::{ItemType, Order, SearchFilter, UploadDate};

use crate::backend::potoken::{NullPoToken, PoTokenProvider};
use crate::backend::{BackendError, BackendOutcome, BackendResult, StreamHandle, YtBackend};
use crate::model::{Channel, Video};

/// rustypipe marks channels with a `Verification` enum; map "has a badge" onto
/// Tube's boolean `verified`.
fn is_verified(v: Verification) -> bool {
    !matches!(v, Verification::None)
}

/// How a Tube [`PoTokenProvider`] choice maps onto rustypipe's builder. Returns
/// a human description for logging; the actual builder wiring is applied in
/// [`RustyPipeBackend::new`].
///
/// * [`NullPoToken`] → `.no_botguard()` (metadata only).
/// * a streaming-capable provider → leave rustypipe's botguard path enabled (or
///   point `.botguard_bin(..)` at the provider's executable). In-process minting
///   via `rustypipe-botguard`'s `Botguard::mint_token` is the alternative when a
///   provider wants full control.
pub fn bind_potoken(provider: &dyn PoTokenProvider) -> &'static str {
    if provider.can_stream() {
        "botguard enabled (streams available)"
    } else {
        "no_botguard (metadata only)"
    }
}

/// The live rustypipe-backed data source.
pub struct RustyPipeBackend {
    rp: RustyPipe,
    rt: tokio::runtime::Runtime,
    potoken_desc: &'static str,
}

impl RustyPipeBackend {
    /// Construct with the default (null) PoToken provider — metadata only.
    pub fn new() -> Result<Self, BackendError> {
        Self::with_potoken(Box::new(NullPoToken))
    }

    /// Construct with an explicit PoToken provider. The provider's capability
    /// selects whether rustypipe's botguard path is enabled.
    pub fn with_potoken(provider: Box<dyn PoTokenProvider>) -> Result<Self, BackendError> {
        // A multi-thread runtime (not current-thread): rustypipe fires internal
        // `tokio::spawn`/blocking work (visitor-data prefetch, the botguard
        // subprocess) that a single-threaded runtime can deadlock on.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| BackendError(format!("tokio runtime: {e}")))?;
        let potoken_desc = bind_potoken(provider.as_ref());
        let can_stream = provider.can_stream();
        // Build the client *inside* the runtime so anything rustypipe wires up
        // at construction (the reqwest pool, cache tasks) has a reactor.
        let rp = rt
            .block_on(async {
                // `no_reporter()`: rustypipe's default reporter writes a
                // `rustypipe_reports/*.json` file into the process cwd on every
                // parser warning. A shipped app must not litter the working
                // directory, so disable it (warnings still degrade gracefully —
                // core fields are extracted regardless).
                let mut builder = RustyPipe::builder().no_reporter();
                if !can_stream {
                    builder = builder.no_botguard();
                }
                builder.build()
            })
            .map_err(|e| BackendError(format!("rustypipe build: {e}")))?;
        Ok(Self { rp, rt, potoken_desc })
    }

    fn block<F, T>(&self, fut: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.rt.block_on(fut)
    }
}

/// Map a rustypipe [`VideoItem`] onto Tube's [`Video`]. Fields rustypipe doesn't
/// carry on a list item (description, likes, comments, sponsor) default empty
/// and are filled in by a later `video_details` / SponsorBlock fetch.
fn map_video_item(it: VideoItem) -> Video {
    let channel = it.channel;
    let (channel_id, channel_name) = channel
        .map(|c| (c.id, c.name))
        .unwrap_or_else(|| (String::new(), "Unknown".into()));
    Video {
        id: it.id.clone(),
        title: it.name,
        dearrow_title: None,
        channel_id: channel_id.clone(),
        channel_name,
        channel_seed: channel_id,
        views: it.view_count.unwrap_or(0),
        published: it.publish_date_txt.unwrap_or_default(),
        duration: it.duration.unwrap_or(0),
        description: it.short_description.unwrap_or_default(),
        likes: 0,
        seed: it.id,
        category: "Search".into(),
        dearrow_thumb: false,
        sponsor: Vec::new(),
        comments: Vec::new(),
        live: it.is_live,
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

/// Pick the smallest H.264-in-MP4 stream we can actually decode, preferring an
/// adaptive video-only DASH stream (its `mdat` fragments are a pure run of video
/// NAL units) over a progressive/muxed one. Returns the stream and whether it is
/// adaptive. Skips DRM-protected tracks — those cannot be decoded here.
fn pick_avc_mp4(player: &RpVideoPlayer) -> Option<(&VideoStream, bool)> {
    let decodable = |s: &VideoStream| {
        s.codec == VideoCodec::Avc1 && s.format == VideoFormat::Mp4 && s.drm_systems.is_empty()
    };
    let area = |s: &VideoStream| s.width as u64 * s.height as u64;

    if let Some(s) = player
        .video_only_streams
        .iter()
        .filter(|s| decodable(s) && s.init_range.is_some() && s.index_range.is_some())
        .min_by_key(|s| area(s))
    {
        return Some((s, true));
    }
    player
        .video_streams
        .iter()
        .filter(|s| decodable(s))
        .min_by_key(|s| area(s))
        .map(|s| (s, false))
}

impl YtBackend for RustyPipeBackend {
    fn name(&self) -> &str {
        self.potoken_desc
    }

    fn search(&self, query: &str) -> BackendOutcome {
        let q = self.rp.query();
        let res = self
            .block(async { q.search::<YouTubeItem, _>(query).await })
            .map_err(|e| BackendError(format!("search: {e}")))?;
        Ok(BackendResult::Videos(videos_from_items(res.items.items)))
    }

    fn video(&self, id: &str) -> BackendOutcome {
        let q = self.rp.query();
        let d = self
            .block(async { q.video_details(id).await })
            .map_err(|e| BackendError(format!("video: {e}")))?;
        let channel_id = d.channel.id.clone();
        let v = Video {
            id: d.id.clone(),
            title: d.name,
            dearrow_title: None,
            channel_id: channel_id.clone(),
            channel_name: d.channel.name,
            channel_seed: channel_id,
            views: d.view_count,
            published: d.publish_date_txt.unwrap_or_default(),
            // Duration comes from the player/streams response; left 0 here and
            // refined when streams are resolved (behind a PoToken).
            duration: 0,
            // `description` is rustypipe's `RichText`; flatten to plain text.
            description: d.description.to_plaintext(),
            likes: d.like_count.unwrap_or(0) as u64,
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
            .block(async { q.channel_videos(id).await })
            .map_err(|e| BackendError(format!("channel: {e}")))?;
        let channel = Channel {
            id: ch.id.clone(),
            name: ch.name.clone(),
            handle: ch.handle.clone().unwrap_or_else(|| ch.id.clone()),
            subscribers: ch.subscriber_count.unwrap_or(0),
            verified: is_verified(ch.verification),
            description: ch.description.clone(),
            seed: ch.id.clone(),
        };
        let uploads = ch.content.items.into_iter().map(map_video_item).collect();
        Ok(BackendResult::Channel { channel: Box::new(channel), uploads })
    }

    fn related(&self, id: &str) -> BackendOutcome {
        // Related ("up next") is a field on the video detail response, not a
        // dedicated endpoint — fetch details and lift `.recommended`.
        let q = self.rp.query();
        let d = self
            .block(async { q.video_details(id).await })
            .map_err(|e| BackendError(format!("related: {e}")))?;
        let vids = d.recommended.items.into_iter().map(map_video_item).collect();
        Ok(BackendResult::Videos(vids))
    }

    fn trending(&self) -> BackendOutcome {
        let q = self.rp.query();
        let items = self
            .block(async { q.trending().await })
            .map_err(|e| BackendError(format!("trending: {e}")))?;
        Ok(BackendResult::Videos(items.into_iter().map(map_video_item).collect()))
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
            .block(async { q.search_filter::<YouTubeItem, _>(query, &filter).await })
            .map_err(|e| BackendError(format!("popular_search {query:?}: {e}")))?;
        Ok(BackendResult::Videos(videos_from_items(res.items.items)))
    }

    fn resolve_video_stream(&self, id: &str) -> Result<StreamHandle, BackendError> {
        // `player()` tries the iOS then TV clients (neither needs a PoToken), so
        // this resolves real, directly-playable stream URLs without BotGuard.
        let q = self.rp.query();
        let player = self
            .block(async { q.player(id).await })
            .map_err(|e| BackendError(format!("player {id}: {e}")))?;
        let (s, adaptive) = pick_avc_mp4(&player).ok_or_else(|| {
            BackendError(format!(
                "no decodable H.264/MP4 stream for {id} (client={:?}; {} video-only + {} muxed \
                 streams — likely VP9/AV1-only or DRM)",
                player.client_type,
                player.video_only_streams.len(),
                player.video_streams.len()
            ))
        })?;
        // googlevideo checks the media request's UA against the client that
        // resolved the URL — replay the exact one rustypipe used.
        let user_agent = self.rp.query().user_agent(player.client_type).into_owned();
        Ok(StreamHandle {
            url: s.url.clone(),
            user_agent,
            itag: s.itag,
            mime: s.mime.clone(),
            is_avc: true,
            is_mp4: true,
            adaptive,
            width: s.width,
            height: s.height,
            fps: s.fps,
            quality: s.quality.clone(),
            content_length: s.size,
            init_range: s.init_range.as_ref().map(|r| (r.start as u64, r.end as u64)),
            index_range: s.index_range.as_ref().map(|r| (r.start as u64, r.end as u64)),
        })
    }
}
