//! The live backend: `YouTube` Music through the pure-Rust `rustypipe` client, compiled
//! under `live` only. `rustypipe` is async, so each query waits on the app runtime's
//! `block_on` and tokio stays inside this module.

#![cfg(feature = "live")]

use rustypipe::client::RustyPipe;
use rustypipe::model::{
    AlbumItem, AlbumType, ArtistId, ArtistItem, MusicItem, MusicPlaylistItem, Thumbnail, TrackItem,
    VideoStream,
};
use tilo_innertube as innertube;

use crate::backend::{
    AlbumHit, AlbumKind, AlbumPage, ArtistAlbum, ArtistHit, ArtistPage, BackendError, HomeContent,
    HomeItem, HomeItemKind, HomeSection, MusicBackend, PlaylistHit, PlaylistPage, ResolveOutcome,
    ResolvedStream, SearchBundle, SearchOutcome, SearchTop, TrackHit,
};

/// No request may hold the serial worker: a timed-out future is dropped and the UI gets
/// a terminal failure it can retry.
const LIVE_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
/// A search answer is a page of rows, not an unbounded allocation.
const SEARCH_RESULT_LIMIT: usize = 96;
const DETAIL_TRACK_LIMIT: usize = 512;
const DETAIL_RELEASE_LIMIT: usize = 96;
const DETAIL_ARTIST_LIMIT: usize = 48;
const DESCRIPTION_LIMIT: usize = 16 * 1024;

pub struct RustyPipeBackend {
    rp: RustyPipe,
    rt: &'static ui::rt::AppRuntime,
}

impl RustyPipeBackend {
    /// Audio resolves through the player client that needs no `PoToken`.
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

/// Where rustypipe keeps its signature cache; never the working tree.
fn cache_dir() -> std::path::PathBuf {
    let base = tilo_paths::env_dir("XDG_CACHE_HOME")
        .or_else(tilo_paths::cache_dir)
        .or_else(tilo_paths::temp_dir)
        .unwrap_or_default();
    let dir = base.join("tilo-music");
    let _ = crate::files::create_dir_all(&dir);
    dir
}

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

/// A rustypipe album type as the UI's coarse kind.
fn map_album_kind(t: AlbumType) -> AlbumKind {
    match t {
        AlbumType::Album => AlbumKind::Album,
        AlbumType::Single => AlbumKind::Single,
        AlbumType::Ep => AlbumKind::Ep,
        _ => AlbumKind::Other,
    }
}

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

fn map_artist_hit(a: ArtistItem) -> ArtistHit {
    ArtistHit {
        id: a.id,
        name: a.name,
        subscriber_count: a.subscriber_count.unwrap_or(0),
        avatar_url: cover_url(&a.avatar, 240),
    }
}

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

/// A display name for a list of artists.
fn artists_name(artists: &[ArtistId], by_va: bool) -> String {
    if let Some(a) = artists.first() {
        a.name.clone()
    } else if by_va {
        "Various Artists".to_string()
    } else {
        "Unknown Artist".to_string()
    }
}

/// The best thumbnail near `px`, empty when the item has no artwork.
fn cover_url(thumbs: &[Thumbnail], px: u32) -> String {
    let Some(t) = thumbs.iter().max_by_key(|t| t.width * t.height) else {
        return String::new();
    };
    resize_google_thumb(&t.url, px)
}

/// The CDN takes the size in its trailing `=w{W}-h{H}` segment, so rewriting it fetches a
/// right-sized JPEG; a URL without that segment is returned unchanged.
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

/// The sound of the smallest whole-file stream, which alone serves the whole track.
fn map_stream(video_id: &str, s: &VideoStream) -> ResolvedStream {
    ResolvedStream {
        video_id: video_id.to_string(),
        url: s.url.clone(),
        mime: s.mime.clone(),
        bitrate: s.average_bitrate,
        itag: s.itag,
        size_bytes: s.size.unwrap_or(0),
        duration_ms: u64::from(s.duration_ms.unwrap_or(0)),
    }
}

/// The catalogue's items as a bundle: a hero card and one bucket per type.
fn bundle_of(items: Vec<MusicItem>) -> SearchBundle {
    let mut bundle = SearchBundle::default();
    for (i, item) in items.into_iter().take(SEARCH_RESULT_LIMIT).enumerate() {
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
    bundle
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
        let res = self.block("music search", innertube::music_search_tracks(&q, query))?;
        Ok(res
            .items
            .items
            .into_iter()
            .take(SEARCH_RESULT_LIMIT)
            .map(map_track_item)
            .collect())
    }

    fn warm(&self) {
        // The suggestion endpoint is the smallest request InnerTube answers.
        let q = self.rp.query();
        if let Err(error) = self.block("warm", innertube::music_search_suggestion(&q, "a")) {
            tracing::debug!(%error, "music: warming the catalogue connection failed");
        }
    }

    fn search_quick(&self, query: &str) -> Result<SearchBundle, BackendError> {
        let q = self.rp.query();
        let res = self.block(
            "music suggestions",
            innertube::music_search_suggestion(&q, query),
        )?;
        Ok(bundle_of(res.items))
    }

    fn search_all(&self, query: &str) -> Result<SearchBundle, BackendError> {
        let q = self.rp.query();
        let res = self.block("music search", innertube::music_search_main(&q, query))?;
        Ok(bundle_of(res.items.items))
    }

    fn resolve_audio(&self, video_id: &str) -> ResolveOutcome {
        let q = self.rp.query();
        let player = self.block("player", innertube::player(&q, video_id))?;
        let best = innertube::whole_file_stream(&player.video_streams, Some(0))
            .ok_or_else(|| BackendError("no playable stream".to_string()))?;
        if !crate::backend::trusted_googlevideo_url(&best.url) {
            return Err(BackendError(
                "player returned an untrusted media origin".to_string(),
            ));
        }
        let stream = map_stream(video_id, best);
        tracing::info!(
            target: "music::resolve",
            video_id,
            itag = stream.itag,
            bitrate = stream.bitrate,
            size_bytes = stream.size_bytes,
            "resolved audio stream"
        );
        Ok(stream)
    }

    fn home(&self) -> Result<HomeContent, BackendError> {
        let q = self.rp.query();
        // Charts drive the primary sections; new albums come from a second endpoint.
        let charts = self.block("music charts", innertube::music_charts(&q, None))?;
        let new_albums = self
            .block("new albums", innertube::music_new_albums(&q))
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
        let a = self.block(
            "music artist",
            innertube::music_artist(&q, artist_id, false),
        )?;
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
        let al = self.block("music album", innertube::music_album(&q, album_id))?;
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
        let pl = self.block("music playlist", innertube::music_playlist(&q, playlist_id))?;
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
        #[cfg(all(windows, target_arch = "aarch64"))]
        {
            let runtime = rquickjs::Runtime::new().expect("native QuickJS runtime");
            let context = rquickjs::Context::full(&runtime).expect("JavaScript context");
            context.with(|ctx| {
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

        {
            let text = "🎵".repeat(DESCRIPTION_LIMIT);
            let bounded = bounded_text(text, DESCRIPTION_LIMIT);
            assert!(bounded.len() <= DESCRIPTION_LIMIT);
            assert!(bounded.is_char_boundary(bounded.len()));
        }
    }
}
