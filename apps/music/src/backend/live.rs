//! Live backend — real YouTube Music via the pure-Rust `rustypipe` InnerTube
//! client. **Compiled only under `--features live`.**
//!
//! `rustypipe` is async (tokio); the [`MusicBackend`] trait is sync because it
//! is driven from Music's blocking [`crate::stream`] worker. This backend
//! therefore owns a small current-thread tokio runtime and `block_on`s each
//! query, keeping the async dependency fully contained here — no other module
//! learns about tokio.
//!
//! ## API surface used (rustypipe 0.11.x — verified against the crate source)
//! * construct: `RustyPipe::builder()…build()?`, then `rp.query()`
//! * `query.music_search_tracks(q).await` → `MusicSearchResult<TrackItem>`
//!   (real YouTube Music song search — titles, artists, album, duration, id)
//! * `query.player(id).await` → `VideoPlayer` whose `audio_streams` carry
//!   directly-playable googlevideo URLs. With no BotGuard configured rustypipe
//!   uses the iOS/TV player clients, which return real stream URLs **without a
//!   PoToken** (see [`crate::backend::potoken`]).
//!
//! Pure Rust: rustypipe embeds `rquickjs` for signature deobfuscation; no
//! ffmpeg / yt-dlp / external JS binary.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::{
    AlbumItem, ArtistId, ArtistItem, AudioCodec, AudioFormat, AudioStream, Thumbnail, TrackItem,
};

use crate::backend::potoken::{NullPoToken, PoTokenProvider};
use crate::backend::{
    AlbumPage, ArtistAlbum, ArtistPage, BackendError, HomeContent, HomeItem, HomeItemKind,
    HomeSection, MusicBackend, ResolveOutcome, ResolvedStream, SearchOutcome, TrackHit,
};

/// The live rustypipe-backed data source.
pub struct RustyPipeBackend {
    rp: RustyPipe,
    rt: tokio::runtime::Runtime,
    potoken_desc: &'static str,
}

impl RustyPipeBackend {
    /// Construct with the default (null) PoToken provider — audio resolves via
    /// the iOS/TV player clients, no token needed.
    pub fn new() -> Result<Self, BackendError> {
        Self::with_potoken(Box::new(NullPoToken))
    }

    /// Construct with an explicit PoToken provider. A provider that cannot
    /// stream keeps rustypipe's BotGuard path disabled (`no_botguard`).
    pub fn with_potoken(provider: Box<dyn PoTokenProvider>) -> Result<Self, BackendError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| BackendError(format!("tokio runtime: {e}")))?;

        let potoken_desc = if provider.can_stream() {
            "youtube-music (live, botguard)"
        } else {
            "youtube-music (live)"
        };

        let mut builder = RustyPipe::builder().storage_dir(cache_dir());
        if !provider.can_stream() {
            builder = builder.no_botguard();
        }
        let rp = builder
            .build()
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

/// Where rustypipe keeps its deobfuscation cache (player.js signature data).
/// A real cache dir — never the working tree — so running the app / probe
/// doesn't leave a `rustypipe_cache.json` in the source. Created if absent.
fn cache_dir() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("qos-music");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Map a rustypipe [`TrackItem`] (a YouTube Music search row) onto Music's
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

/// YouTube's image CDN encodes the requested size in a trailing `=w{W}-h{H}-…`
/// segment (lh3/yt3/googleusercontent hosts). Rewriting it fetches a
/// right-sized JPEG (`-rj`) instead of a 60px avatar or a 1200px original —
/// smaller downloads, sharp tiles. URLs without that segment (e.g.
/// `i.ytimg.com/vi/<id>/hqdefault.jpg`) are returned unchanged.
fn resize_google_thumb(url: &str, px: u32) -> String {
    if let Some(eq) = url.rfind('=') {
        let tail = &url[eq + 1..];
        if tail.starts_with('w') || tail.starts_with('s') {
            return format!("{}=w{px}-h{px}-l90-rj", &url[..eq]);
        }
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
    let subtitle = match a.year {
        Some(y) => format!("{artist} \u{00b7} {y}"),
        None => artist.clone(),
    };
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
        Some(n) => format!("{} subscribers", human_count(n)),
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

fn human_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.0}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn codec_str(c: AudioCodec) -> &'static str {
    match c {
        AudioCodec::Mp4a => "mp4a",
        AudioCodec::Opus => "opus",
        AudioCodec::Ac3 => "ac-3",
        AudioCodec::Ec3 => "ec-3",
        AudioCodec::Unknown => "unknown",
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
///      GStreamer install; Opus/WebM is the fallback.
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
        duration_ms: s.duration_ms.unwrap_or(0) as u64,
        fallback_url: None,
    }
}

impl MusicBackend for RustyPipeBackend {
    fn name(&self) -> &str {
        self.potoken_desc
    }

    fn is_live(&self) -> bool {
        true
    }

    fn search_tracks(&self, query: &str) -> SearchOutcome {
        let q = self.rp.query();
        let res = self
            .block(async { q.music_search_tracks(query).await })
            .map_err(|e| BackendError(format!("music search: {e}")))?;
        Ok(res.items.items.into_iter().map(map_track_item).collect())
    }

    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome {
        // Reliable base: rustypipe's direct iOS/TV googlevideo URL. It always
        // renders, but is PoToken/SABR-gated — it only serves the first ~1-minute
        // segment (the "only a minute plays" bug).
        let q = self.rp.query();
        let player = self
            .block(async { q.player(video_id).await })
            .map_err(|e| BackendError(format!("player: {e}")))?;
        let best = best_audio(&player.audio_streams)
            .ok_or_else(|| BackendError("no playable audio streams".to_string()))?;
        let direct = map_stream(video_id, best);

        // Prefer a fully-rangeable Invidious `local` proxy so the WHOLE track
        // streams (it serves arbitrary byte ranges over the complete file). Keep
        // the direct URL as a fallback the engine uses if the (public, sometimes
        // flaky) proxy yields no bytes — so playback always works.
        if let Some(mut proxy) = crate::backend::proxy::resolve_rangeable(video_id) {
            proxy.duration_ms = direct.duration_ms;
            if proxy.size_bytes == 0 {
                proxy.size_bytes = direct.size_bytes;
            }
            proxy.fallback_url = Some(direct.url);
            return Ok(proxy);
        }
        Ok(direct)
    }

    fn home(&self) -> Result<HomeContent, BackendError> {
        let q = self.rp.query();
        // Charts drive the primary sections (top + trending songs, top artists,
        // genre playlists); new albums come from a second endpoint. Both are
        // real YouTube-Music browse responses.
        let charts = self
            .block(async { q.music_charts(None).await })
            .map_err(|e| BackendError(format!("music charts: {e}")))?;
        let new_albums = self
            .block(async { q.music_new_albums().await })
            .unwrap_or_default();

        let mut sections = Vec::new();
        let take_tracks = |v: Vec<TrackItem>, n: usize| {
            v.into_iter().take(n).map(track_home_item).collect::<Vec<_>>()
        };
        let top = take_tracks(charts.top_tracks, 12);
        if !top.is_empty() {
            sections.push(HomeSection { title: "Top songs today".into(), items: top });
        }
        let new_items: Vec<HomeItem> = new_albums.into_iter().take(12).map(album_home_item).collect();
        if !new_items.is_empty() {
            sections.push(HomeSection { title: "New releases".into(), items: new_items });
        }
        let trending = take_tracks(charts.trending_tracks, 12);
        if !trending.is_empty() {
            sections.push(HomeSection { title: "Trending".into(), items: trending });
        }
        let artists: Vec<HomeItem> = charts.artists.into_iter().take(12).map(artist_home_item).collect();
        if !artists.is_empty() {
            sections.push(HomeSection { title: "Top artists".into(), items: artists });
        }

        if sections.is_empty() {
            return Err(BackendError("home content was empty".into()));
        }
        Ok(HomeContent { sections })
    }

    fn artist(&self, artist_id: &str) -> Result<ArtistPage, BackendError> {
        let q = self.rp.query();
        let a = self
            .block(async { q.music_artist(artist_id, false).await })
            .map_err(|e| BackendError(format!("music artist: {e}")))?;
        let tracks = a.tracks.into_iter().map(map_track_item).collect();
        let albums = a
            .albums
            .into_iter()
            .map(|al| ArtistAlbum {
                title: al.name,
                year: al.year.unwrap_or(0) as u32,
                cover_url: cover_url(&al.cover, 300),
                id: al.id,
            })
            .collect();
        Ok(ArtistPage {
            id: a.id,
            name: a.name,
            subscriber_count: a.subscriber_count.unwrap_or(0),
            description: a.description.unwrap_or_default(),
            header_url: cover_url(&a.header_image, 600),
            tracks,
            albums,
        })
    }

    fn album(&self, album_id: &str) -> Result<AlbumPage, BackendError> {
        let q = self.rp.query();
        let al = self
            .block(async { q.music_album(album_id).await })
            .map_err(|e| BackendError(format!("music album: {e}")))?;
        let artist_name = artists_name(&al.artists, al.by_va);
        let cover = cover_url(&al.cover, 400);
        let tracks = al.tracks.into_iter().map(map_track_item).collect();
        Ok(AlbumPage {
            id: al.id,
            title: al.name,
            artist_id: al.artist_id.unwrap_or_default(),
            artist_name,
            year: al.year.unwrap_or(0) as u32,
            cover_url: cover,
            tracks,
        })
    }
}
