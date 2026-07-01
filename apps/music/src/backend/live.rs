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
use rustypipe::model::{AudioCodec, AudioFormat, AudioStream, TrackItem};

use crate::backend::potoken::{NullPoToken, PoTokenProvider};
use crate::backend::{
    BackendError, MusicBackend, ResolveOutcome, ResolvedStream, SearchOutcome, TrackHit,
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
    TrackHit {
        video_id: it.id,
        title: it.name,
        artist,
        artist_id,
        album,
        album_id,
        duration_s: it.duration.unwrap_or(0),
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
        let q = self.rp.query();
        let player = self
            .block(async { q.player(video_id).await })
            .map_err(|e| BackendError(format!("player: {e}")))?;
        let best = best_audio(&player.audio_streams)
            .ok_or_else(|| BackendError("no playable audio streams".to_string()))?;
        Ok(map_stream(video_id, best))
    }
}
