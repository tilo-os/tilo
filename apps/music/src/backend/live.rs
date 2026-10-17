//! Live backend — `YouTube` Music via the pure-Rust `rustypipe` `InnerTube`
//! client. **Compiled only under `--features live`.**
//!
//! `rustypipe` is async (tokio); the [`MusicBackend`] trait is sync because it
//! is driven from Music's blocking [`crate::stream`] worker. This backend
//! therefore waits on each query with the shared app runtime's `block_on`
//! (the query is polled on the worker while the runtime thread drives its
//! sockets and timers), keeping the async dependency fully contained here —
//! no other module learns about tokio.
//!
//! ## API surface used (rustypipe 0.11.x — verified against the crate source)
//! * construct: `RustyPipe::builder()…build()?`, then `rp.query()`
//! * `query.music_search_tracks(q).await` → `MusicSearchResult<TrackItem>`
//!   (`YouTube` Music song search — titles, artists, album, duration, id)
//! * `query.player(id).await` → `VideoPlayer` whose `audio_streams` carry
//!   directly-playable googlevideo URLs. With no `BotGuard` configured rustypipe
//!   uses the iOS/TV player clients, which return stream URLs **without a
//!   `PoToken`**, so the client is built with `no_botguard`.
//!
//! Search and metadata stay pure Rust through rustypipe. Stream resolution
//! first queries the keyless Android VR `InnerTube` client directly, then falls
//! back to rustypipe's direct player response.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::{
    AlbumItem, AlbumType, ArtistId, ArtistItem, AudioCodec, AudioFormat, AudioStream, MusicItem,
    MusicPlaylistItem, Thumbnail, TrackItem,
};

use crate::backend::{
    AlbumHit, AlbumKind, AlbumPage, ArtistAlbum, ArtistHit, ArtistPage, BackendError, HomeContent,
    HomeItem, HomeItemKind, HomeSection, MusicBackend, PlaylistHit, PlaylistPage, ResolveOutcome,
    ResolvedStream, SearchBundle, SearchOutcome, SearchTop, TrackHit,
};

/// No metadata or player request may monopolise Music's serial background
/// worker indefinitely. Dropping the timed-out future cancels rustypipe's HTTP
/// work; the UI receives an ordinary terminal failure and can retry.
const LIVE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// External result collections are presentation pages, not an authority to
/// allocate an unbounded response into the app model.
const SEARCH_RESULT_LIMIT: usize = 96;
const DETAIL_TRACK_LIMIT: usize = 512;
const DETAIL_RELEASE_LIMIT: usize = 96;
const DETAIL_ARTIST_LIMIT: usize = 48;
const DESCRIPTION_LIMIT: usize = 16 * 1024;

/// The live rustypipe-backed data source.
pub struct RustyPipeBackend {
    rp: RustyPipe,
    rt: &'static ui::rt::AppRuntime,
}

impl RustyPipeBackend {
    /// Audio resolves via the iOS/TV player clients, which need no `PoToken`,
    /// so rustypipe's `BotGuard` path stays disabled.
    ///
    /// # Errors
    ///
    /// Returns a [`BackendError`] if the app runtime or the rustypipe
    /// client fails to build.
    pub fn new() -> Result<Self, BackendError> {
        let rt = ui::rt::try_app().map_err(|e| BackendError(format!("tokio runtime: {e}")))?;
        // rustypipe's reqwest client takes the process-default TLS provider.
        tilo_net::install_crypto_provider();
        let rp = RustyPipe::builder()
            .storage_dir(cache_dir())
            .no_botguard()
            .build()
            .map_err(|e| BackendError(format!("rustypipe build: {e}")))?;
        Ok(Self { rp, rt })
    }

    fn block<F, T, E>(&self, operation: &str, fut: F) -> Result<T, BackendError>
    where
        F: std::future::Future<Output = Result<T, E>>,
        E: std::fmt::Display,
    {
        block_with_deadline(self.rt, operation, LIVE_REQUEST_TIMEOUT, fut)
    }
}

fn block_with_deadline<F, T, E>(
    runtime: &ui::rt::AppRuntime,
    operation: &str,
    deadline: std::time::Duration,
    future: F,
) -> Result<T, BackendError>
where
    F: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    runtime
        .block_on(async {
            tokio::time::timeout(deadline, future)
                .await
                .map_err(|_| {
                    BackendError(format!(
                        "{operation} timed out after {} seconds",
                        deadline.as_secs_f64()
                    ))
                })?
                .map_err(|error| BackendError(format!("{operation}: {error}")))
        })
        .map_err(|error| BackendError(format!("{operation}: {error}")))?
}

fn bounded_text(mut value: String, limit: usize) -> String {
    if value.len() <= limit {
        return value;
    }
    let boundary = value
        .char_indices()
        .take_while(|(index, _)| *index <= limit)
        .map(|(index, _)| index)
        .last()
        .unwrap_or(0);
    value.truncate(boundary);
    value
}

/// Where rustypipe keeps its deobfuscation cache (player.js signature data).
/// A real cache dir — never the working tree — so running the app / probe
/// doesn't leave a `rustypipe_cache.json` in the source. Created if absent.
fn cache_dir() -> std::path::PathBuf {
    // An absolute XDG_CACHE_HOME wins on every platform (sandboxed launches
    // set one); otherwise the platform's own cache directory.
    let base = tilo_paths::env_dir("XDG_CACHE_HOME")
        .or_else(tilo_paths::cache_dir)
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default();
    let dir = base.join("tilo-music");
    let _ = crate::files::create_dir_all(&dir);
    dir
}

/// Map a rustypipe [`TrackItem`] (a `YouTube` Music search row) onto Music's
/// toolkit-free [`TrackHit`].
fn map_track_item(it: TrackItem) -> TrackHit {
    let (artist, artist_id) = if let Some(a) = it.artists.into_iter().next() {
        (a.name, a.id)
    } else if it.by_va {
        ("Various Artists".to_string(), None)
    } else {
        ("Unknown Artist".to_string(), None)
    };
    let (album, album_id) = match it.album {
        Some(al) => (Some(al.name), Some(al.id)),
        None => (None, None),
    };
    let cover_url = cover_url(&it.cover, 300);
    TrackHit {
        video_id: it.id,
        title: it.name,
        artist,
        artist_id,
        album,
        album_id,
        duration_s: it.duration.unwrap_or(0),
        cover_url,
    }
}

/// Map a rustypipe [`AlbumType`] onto the UI's coarse [`AlbumKind`] so a
/// discography can be sectioned (Albums vs. Singles & EPs).
fn map_album_kind(t: AlbumType) -> AlbumKind {
    match t {
        AlbumType::Album => AlbumKind::Album,
        AlbumType::Single => AlbumKind::Single,
        AlbumType::Ep => AlbumKind::Ep,
        _ => AlbumKind::Other,
    }
}

/// Map a rustypipe [`AlbumItem`] (a search / discography row) onto [`AlbumHit`].
fn map_album_hit(a: AlbumItem) -> AlbumHit {
    let artist = artists_name(&a.artists, a.by_va);
    AlbumHit {
        id: a.id,
        title: a.name,
        artist,
        artist_id: a.artist_id,
        year: u32::from(a.year.unwrap_or(0)),
        kind: map_album_kind(a.album_type),
        cover_url: cover_url(&a.cover, 300),
    }
}

/// Map a rustypipe [`ArtistItem`] onto [`ArtistHit`].
fn map_artist_hit(a: ArtistItem) -> ArtistHit {
    ArtistHit {
        id: a.id,
        name: a.name,
        subscriber_count: a.subscriber_count.unwrap_or(0),
        avatar_url: cover_url(&a.avatar, 240),
    }
}

/// Map a rustypipe [`MusicPlaylistItem`] onto [`PlaylistHit`].
fn map_playlist_hit(p: MusicPlaylistItem) -> PlaylistHit {
    let subtitle = match (&p.channel, p.from_ytm) {
        (Some(ch), _) => ch.name.clone(),
        (None, true) => "YouTube Music".to_string(),
        (None, false) => "Playlist".to_string(),
    };
    PlaylistHit {
        id: p.id,
        title: p.name,
        subtitle,
        track_count: p.track_count.unwrap_or(0),
        cover_url: cover_url(&p.thumbnail, 300),
    }
}

/// Display name for a list of artists (falls back to VA / Unknown).
fn artists_name(artists: &[ArtistId], by_va: bool) -> String {
    if let Some(a) = artists.first() {
        a.name.clone()
    } else if by_va {
        "Various Artists".to_string()
    } else {
        "Unknown Artist".to_string()
    }
}

/// Pick the best cover thumbnail and normalise it to roughly `px` pixels.
/// Empty when the item carries no artwork.
fn cover_url(thumbs: &[Thumbnail], px: u32) -> String {
    let Some(t) = thumbs.iter().max_by_key(|t| t.width * t.height) else {
        return String::new();
    };
    resize_google_thumb(&t.url, px)
}

/// `YouTube`'s image CDN encodes the requested size in a trailing `=w{W}-h{H}-…`
/// segment (lh3/yt3/googleusercontent hosts). Rewriting it fetches a
/// right-sized JPEG (`-rj`) instead of a 60px avatar or a 1200px original —
/// smaller downloads, sharp tiles. URLs without that segment (e.g.
/// `i.ytimg.com/vi/<id>/hqdefault.jpg`) are returned unchanged.
fn resize_google_thumb(url: &str, px: u32) -> String {
    if let Some(eq) = url.rfind('=')
        && let Some(tail) = url.get(eq + 1..)
        && (tail.starts_with('w') || tail.starts_with('s'))
        && let Some(base) = url.get(..eq)
    {
        return format!("{base}=w{px}-h{px}-l90-rj");
    }
    url.to_string()
}

fn track_home_item(it: TrackItem) -> HomeItem {
    let hit = map_track_item(it);
    HomeItem {
        id: hit.video_id.clone(),
        title: hit.title,
        subtitle: hit.artist.clone(),
        cover_url: hit.cover_url,
        kind: HomeItemKind::Track,
        video_id: hit.video_id,
        artist_id: hit.artist_id.unwrap_or_default(),
        artist_name: hit.artist,
        album_id: hit.album_id.unwrap_or_default(),
        album_title: hit.album.unwrap_or_default(),
        duration_s: hit.duration_s,
    }
}

fn album_home_item(a: AlbumItem) -> HomeItem {
    let artist = artists_name(&a.artists, a.by_va);
    let subtitle = artist.clone();
    HomeItem {
        id: a.id,
        title: a.name,
        subtitle,
        cover_url: cover_url(&a.cover, 300),
        kind: HomeItemKind::Album,
        video_id: String::new(),
        artist_id: a.artist_id.unwrap_or_default(),
        artist_name: artist,
        album_id: String::new(),
        album_title: String::new(),
        duration_s: 0,
    }
}

fn artist_home_item(a: ArtistItem) -> HomeItem {
    let subtitle = match a.subscriber_count {
        Some(n) => format!("{} subscribers", ui::format::compact_count(n)),
        None => "Artist".to_string(),
    };
    HomeItem {
        id: a.id,
        title: a.name,
        subtitle,
        cover_url: cover_url(&a.avatar, 240),
        kind: HomeItemKind::Artist,
        video_id: String::new(),
        artist_id: String::new(),
        artist_name: String::new(),
        album_id: String::new(),
        album_title: String::new(),
        duration_s: 0,
    }
}

fn codec_str(c: AudioCodec) -> &'static str {
    match c {
        AudioCodec::Mp4a => "mp4a",
        AudioCodec::Opus => "opus",
        AudioCodec::Ac3 => "ac-3",
        AudioCodec::Ec3 => "ec-3",
        _ => "unknown",
    }
}

fn format_str(f: AudioFormat) -> &'static str {
    match f {
        AudioFormat::M4a => "m4a",
        AudioFormat::Webm => "webm",
        _ => "unknown",
    }
}

/// Pick the best audio stream to actually play.
///
/// Preference order:
///   1. Non-DRM streams only (DRM tracks aren't playable without a license).
///   2. Prefer the M4A/AAC container — the most broadly decodable by a stock
///      `GStreamer` install; Opus/WebM is the fallback.
///   3. Within the chosen container, the highest average bitrate.
///
/// Returns `None` only if there are no non-DRM audio streams at all.
pub fn best_audio(streams: &[AudioStream]) -> Option<&AudioStream> {
    let playable = || streams.iter().filter(|s| s.drm_systems.is_empty());
    let by_bitrate = |a: &&AudioStream, b: &&AudioStream| a.average_bitrate.cmp(&b.average_bitrate);

    playable()
        .filter(|s| s.format == AudioFormat::M4a)
        .max_by(by_bitrate)
        .or_else(|| playable().max_by(by_bitrate))
}

fn map_stream(video_id: &str, s: &AudioStream) -> ResolvedStream {
    ResolvedStream {
        video_id: video_id.to_string(),
        url: s.url.clone(),
        mime: s.mime.clone(),
        codec: codec_str(s.codec).to_string(),
        format: format_str(s.format).to_string(),
        bitrate: s.average_bitrate,
        itag: s.itag,
        size_bytes: s.size,
        duration_ms: u64::from(s.duration_ms.unwrap_or(0)),
        fallback_url: None,
    }
}

/// Standard `YouTube` audio itags, best-decodable first: m4a/AAC 128k (any
/// stock `GStreamer` decodes it), then webm/opus 160k. Present for effectively
/// every track, so the proxy is probed directly without first asking the
/// (often Cloudflare-gated) metadata endpoint for the itag list.
const KNOWN_AUDIO_ITAGS: &[(u32, &str, &str, &str, u32)] = &[
    // (itag, mime, codec, format, bitrate)
    (
        140,
        "audio/mp4; codecs=\"mp4a.40.2\"",
        "mp4a",
        "m4a",
        128_000,
    ),
    (251, "audio/webm; codecs=\"opus\"", "opus", "webm", 160_000),
];

/// A fully-rangeable Invidious-proxied audio stream for `video_id`, when an
/// instance's proxy serves real media within the budget.
async fn resolve_rangeable(video_id: &str) -> Option<ResolvedStream> {
    let (url, &(itag, mime, codec, format, bitrate)) =
        tilo_invidious::first_serving(video_id, KNOWN_AUDIO_ITAGS, |f| f.0).await?;
    Some(ResolvedStream {
        video_id: video_id.to_string(),
        url,
        mime: mime.to_string(),
        codec: codec.to_string(),
        format: format.to_string(),
        bitrate,
        itag,
        size_bytes: 0,
        duration_ms: 0,
        fallback_url: None,
    })
}

impl MusicBackend for RustyPipeBackend {
    fn name(&self) -> &'static str {
        "youtube-music (live)"
    }

    fn is_live(&self) -> bool {
        true
    }

    fn search_tracks(&self, query: &str) -> SearchOutcome {
        let q = self.rp.query();
        let res = self.block("music search", async { q.music_search_tracks(query).await })?;
        Ok(res
            .items
            .items
            .into_iter()
            .take(SEARCH_RESULT_LIMIT)
            .map(map_track_item)
            .collect())
    }

    fn search_all(&self, query: &str) -> Result<SearchBundle, BackendError> {
        let q = self.rp.query();
        let res = self.block("music search", async { q.music_search_main(query).await })?;

        let mut bundle = SearchBundle::default();
        // YouTube Music returns the top-result card first, so the first item is
        // the hero. Bucket the rest by type, preserving ranking order.
        for (i, item) in res
            .items
            .items
            .into_iter()
            .take(SEARCH_RESULT_LIMIT)
            .enumerate()
        {
            match item {
                MusicItem::Track(t) => {
                    let hit = map_track_item(t);
                    if i == 0 && bundle.top.is_none() {
                        bundle.top = Some(SearchTop::Track(hit.clone()));
                    }
                    bundle.tracks.push(hit);
                }
                MusicItem::Album(a) => {
                    let hit = map_album_hit(a);
                    if i == 0 && bundle.top.is_none() {
                        bundle.top = Some(SearchTop::Album(hit.clone()));
                    }
                    bundle.albums.push(hit);
                }
                MusicItem::Artist(a) => {
                    let hit = map_artist_hit(a);
                    if i == 0 && bundle.top.is_none() {
                        bundle.top = Some(SearchTop::Artist(hit.clone()));
                    }
                    bundle.artists.push(hit);
                }
                MusicItem::Playlist(p) => {
                    let hit = map_playlist_hit(p);
                    if i == 0 && bundle.top.is_none() {
                        bundle.top = Some(SearchTop::Playlist(hit.clone()));
                    }
                    bundle.playlists.push(hit);
                }
                MusicItem::User(_) => {}
            }
        }
        Ok(bundle)
    }

    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome {
        // Prefer the independent full-track resolver before asking InnerTube's
        // player endpoint. YouTube can reject rustypipe's iOS/TV player client
        // while the Android VR client still works; resolving the fallback
        // first used to turn that recoverable condition into a hard failure.
        // An Android VR URL is already fully rangeable, so it needs no truncating
        // direct-stream fallback.
        if let Some(full) = crate::backend::android_vr::resolve_full_audio(video_id) {
            tracing::info!(target: "music::resolve", itag = full.itag, "using Rust-native Android VR full-track URL");
            return Ok(full);
        }

        // Fallback base: rustypipe's direct iOS/TV googlevideo URL. It renders
        // when the player endpoint accepts this client, but is PoToken/SABR-
        // gated and may only serve the first ~1-minute segment.
        let q = self.rp.query();
        let player = self.block("player", async { q.player(video_id).await })?;
        let best = best_audio(&player.audio_streams)
            .ok_or_else(|| BackendError("no playable audio streams".to_string()))?;
        if !crate::backend::trusted_googlevideo_url(&best.url) {
            return Err(BackendError(
                "player returned an untrusted media origin".to_string(),
            ));
        }
        let direct = map_stream(video_id, best);
        tracing::info!(
            target: "music::resolve",
            video_id,
            itag = direct.itag,
            codec = %direct.codec,
            bitrate = direct.bitrate,
            size_bytes = direct.size_bytes,
            "resolved direct googlevideo audio stream"
        );

        // Otherwise try a fully-rangeable Invidious `local` proxy, but only if that
        // proxy genuinely serves media (validated). Public instances are mostly
        // dead / anti-bot-gated, so this usually returns `None`.
        // The direct URL is always carried as the engine's fallback too.
        if let Some(mut proxy) = self.rt.block_on(resolve_rangeable(video_id)).ok().flatten() {
            proxy.duration_ms = direct.duration_ms;
            if proxy.size_bytes == 0 {
                proxy.size_bytes = direct.size_bytes;
            }
            proxy.fallback_url = Some(direct.url);
            tracing::info!(target: "music::resolve", "using rangeable proxy (direct URL as fallback)");
            return Ok(proxy);
        }
        tracing::info!(target: "music::resolve", "no working proxy — playing direct googlevideo URL");
        Ok(direct)
    }

    fn home(&self) -> Result<HomeContent, BackendError> {
        let q = self.rp.query();
        // Charts drive the primary sections (top + trending songs, top artists,
        // genre playlists); new albums come from a second endpoint. Both are
        // real YouTube-Music browse responses.
        let charts = self.block("music charts", async { q.music_charts(None).await })?;
        let new_albums = self
            .block("new albums", async { q.music_new_albums().await })
            .unwrap_or_default();

        let mut sections = Vec::new();
        let take_tracks = |v: Vec<TrackItem>, n: usize| {
            v.into_iter()
                .take(n)
                .map(track_home_item)
                .collect::<Vec<_>>()
        };
        let top = take_tracks(charts.top_tracks, 12);
        if !top.is_empty() {
            sections.push(HomeSection {
                title: "Top songs today".into(),
                items: top,
            });
        }
        let new_items: Vec<HomeItem> = new_albums
            .into_iter()
            .take(12)
            .map(album_home_item)
            .collect();
        if !new_items.is_empty() {
            sections.push(HomeSection {
                title: "New releases".into(),
                items: new_items,
            });
        }
        let trending = take_tracks(charts.trending_tracks, 12);
        if !trending.is_empty() {
            sections.push(HomeSection {
                title: "Trending".into(),
                items: trending,
            });
        }
        let artists: Vec<HomeItem> = charts
            .artists
            .into_iter()
            .take(12)
            .map(artist_home_item)
            .collect();
        if !artists.is_empty() {
            sections.push(HomeSection {
                title: "Top artists".into(),
                items: artists,
            });
        }

        if sections.is_empty() {
            return Err(BackendError("home content was empty".into()));
        }
        Ok(HomeContent { sections })
    }

    fn artist(&self, artist_id: &str) -> Result<ArtistPage, BackendError> {
        let q = self.rp.query();
        let a = self.block("music artist", async {
            q.music_artist(artist_id, false).await
        })?;
        let tracks = a
            .tracks
            .into_iter()
            .take(DETAIL_TRACK_LIMIT)
            .map(map_track_item)
            .collect();
        let albums = a
            .albums
            .into_iter()
            .take(DETAIL_RELEASE_LIMIT)
            .map(|al| ArtistAlbum {
                title: al.name,
                year: u32::from(al.year.unwrap_or(0)),
                kind: map_album_kind(al.album_type),
                cover_url: cover_url(&al.cover, 300),
                id: al.id,
            })
            .collect();
        let similar = a
            .similar_artists
            .into_iter()
            .take(DETAIL_ARTIST_LIMIT)
            .map(map_artist_hit)
            .collect();
        Ok(ArtistPage {
            id: a.id,
            name: a.name,
            subscriber_count: a.subscriber_count.unwrap_or(0),
            description: bounded_text(a.description.unwrap_or_default(), DESCRIPTION_LIMIT),
            wikipedia_url: a.wikipedia_url.unwrap_or_default(),
            header_url: cover_url(&a.header_image, 600),
            tracks,
            albums,
            similar,
            provider_followed: None,
        })
    }

    fn album(&self, album_id: &str) -> Result<AlbumPage, BackendError> {
        let q = self.rp.query();
        let al = self.block("music album", async { q.music_album(album_id).await })?;
        let artist_name = artists_name(&al.artists, al.by_va);
        let cover = cover_url(&al.cover, 400);
        let tracks = al
            .tracks
            .into_iter()
            .take(DETAIL_TRACK_LIMIT)
            .map(map_track_item)
            .collect();
        Ok(AlbumPage {
            id: al.id,
            title: al.name,
            artist_id: al.artist_id.unwrap_or_default(),
            artist_name,
            year: u32::from(al.year.unwrap_or(0)),
            cover_url: cover,
            tracks,
        })
    }

    fn playlist(&self, playlist_id: &str) -> Result<PlaylistPage, BackendError> {
        let q = self.rp.query();
        let pl = self.block("music playlist", async {
            q.music_playlist(playlist_id).await
        })?;
        let owner = pl.channel.as_ref().map_or_else(
            || {
                if pl.from_ytm {
                    "YouTube Music".into()
                } else {
                    "Playlist".into()
                }
            },
            |c| c.name.clone(),
        );
        let subtitle = owner;
        let description = bounded_text(
            pl.description
                .as_ref()
                .map(rustypipe::model::richtext::ToPlaintext::to_plaintext)
                .unwrap_or_default(),
            DESCRIPTION_LIMIT,
        );
        let cover = cover_url(&pl.thumbnail, 400);
        let tracks = pl
            .tracks
            .items
            .into_iter()
            .take(DETAIL_TRACK_LIMIT)
            .map(map_track_item)
            .collect();
        Ok(PlaylistPage {
            id: pl.id,
            title: pl.name,
            subtitle,
            description,
            cover_url: cover,
            tracks,
        })
    }
}

#[cfg(test)]
mod bounded_transport_tests {
    use super::*;

    #[test]
    fn live_transport_is_bounded_utf8_safe_and_signature_capable() {
        // Phase `native_quickjs_bindings_execute_stream_signature_operations`.
        #[cfg(all(windows, target_arch = "aarch64"))]
        {
            let runtime = rquickjs::Runtime::new().expect("native QuickJS runtime");
            let context = rquickjs::Context::full(&runtime).expect("JavaScript context");
            context.with(|ctx| {
                // Stream resolvers pass strings into JavaScript and read strings
                // back. Exercise that ABI, including allocation and Unicode.
                let result: String = ctx
                    .eval("Array.from('a🎵bc').reverse().join('')")
                    .expect("evaluate signature-style string transform");
                assert_eq!(result, "cb🎵a");
                assert!(
                    ctx.eval::<String, _>("throw new Error('invalid signature')")
                        .is_err()
                );
            });
        }

        // Phase `live_request_deadline_cancels_a_stalled_future`.
        {
            let result = block_with_deadline(
                ui::rt::app(),
                "test request",
                std::time::Duration::from_millis(5),
                async {
                    tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                    Ok::<_, &str>(())
                },
            );
            assert!(
                result
                    .expect_err("stalled future must be cancelled")
                    .to_string()
                    .contains("timed out")
            );
        }

        // Phase `external_text_is_utf8_safe_and_bounded`.
        {
            let text = "🎵".repeat(DESCRIPTION_LIMIT);
            let bounded = bounded_text(text, DESCRIPTION_LIMIT);
            assert!(bounded.len() <= DESCRIPTION_LIMIT);
            assert!(bounded.is_char_boundary(bounded.len()));
        }
    }
}
