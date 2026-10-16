//! Search-type seam — Music result descriptors and canonical resource actions.
//!
//! Music must *not* build a bespoke OS-wide search. Instead it exposes clean
//! descriptors the global-search framework can register, so a song / album /
//! artist / playlist shows up in the OS search palette with a cover, a title
//! and a subtitle — and activating a hit resolves a canonical resource action
//! that plays the song or opens the album/artist/playlist page.
//!
//! This module defines Music's provider-side descriptors. The toolkit-facing
//! library provider below supplies the shared palette with canonical actions:
//!
//! * [`SearchableType`] — the static descriptor (stable id, display name,
//!   whether hits carry a cover). One per app-provided result type.
//! * [`SearchHit`] — one result row: title, subtitle, a real cover URL (live)
//!   or a procedural seed (offline), plus the deep link that opens it.
//! * [`MusicSearchProvider`] — turns a query into [`SearchHit`]s by delegating
//!   to an explicitly supplied [`crate::backend::MusicBackend`]. The shipping
//!   build enables the live backend; tests can supply the offline sample.
//! * [`library_entries`] — the user's **own library** (synced across their
//!   devices) as the library resource's search entries, so a saved album or a
//!   personal playlist is findable even on the hermetic/offline build.
//!
//! Descriptor registration is a one-liner
//! (`for t in music::searchable_types() { registry.register(t) }` plus a
//! provider query with no reach into app internals. Search activation itself
//! goes through the shared operation broker and Music's exported resource tree.

use ui::icons::Icon;
use ui::tilo_resource::{SearchEntry, SearchWeight};

use crate::backend::{MusicBackend, SearchTop};
use crate::model::Library;

/// Stable ids for Music's four searchable result types. The framework uses them
/// to deduplicate providers and persist per-type preferences.
pub const SONG_TYPE_ID: &str = "music.song";
pub const ALBUM_TYPE_ID: &str = "music.album";
pub const ARTIST_TYPE_ID: &str = "music.artist";
pub const PLAYLIST_TYPE_ID: &str = "music.playlist";

/// A static descriptor of a result type Music contributes to global search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableType {
    /// Stable id (e.g. [`SONG_TYPE_ID`]).
    pub id: &'static str,
    /// Human, localisable display name for the palette section header.
    pub display_name: &'static str,
    /// Whether hits of this type carry a cover, so the palette reserves image
    /// space. Artists render a round avatar; the rest a square cover.
    pub has_thumbnail: bool,
}

/// The descriptor Music registers for song results.
pub fn song_searchable_type() -> SearchableType {
    SearchableType {
        id: SONG_TYPE_ID,
        display_name: "Song",
        has_thumbnail: true,
    }
}

/// The descriptor Music registers for album results.
pub fn album_searchable_type() -> SearchableType {
    SearchableType {
        id: ALBUM_TYPE_ID,
        display_name: "Album",
        has_thumbnail: true,
    }
}

/// The descriptor Music registers for artist results.
pub fn artist_searchable_type() -> SearchableType {
    SearchableType {
        id: ARTIST_TYPE_ID,
        display_name: "Artist",
        has_thumbnail: true,
    }
}

/// The descriptor Music registers for playlist results.
pub fn playlist_searchable_type() -> SearchableType {
    SearchableType {
        id: PLAYLIST_TYPE_ID,
        display_name: "Playlist",
        has_thumbnail: true,
    }
}

/// Every result type Music contributes, in the order the palette should section
/// them (songs first, then albums, artists, playlists).
pub fn all_searchable_types() -> Vec<SearchableType> {
    vec![
        song_searchable_type(),
        album_searchable_type(),
        artist_searchable_type(),
        playlist_searchable_type(),
    ]
}

/// A single global-search result row contributed by Music.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// The result type this hit belongs to (e.g. [`SONG_TYPE_ID`]).
    pub type_id: &'static str,
    /// Primary line (song / album / artist / playlist title).
    pub title: String,
    /// Secondary line: the artist, the owner, "N songs", a duration — whatever
    /// disambiguates the hit at a glance.
    pub subtitle: String,
    /// Real cover-art URL (`YouTube` image CDN) when the live backend provides
    /// one. Empty for own-library / offline hits — the palette then renders the
    /// procedural tile keyed by [`Self::thumbnail_seed`], so no image bytes
    /// need cross the boundary.
    pub cover_url: String,
    /// Seed for Music's deterministic cover gradient — the palette can ask
    /// Music to render it when [`Self::cover_url`] is empty.
    pub thumbnail_seed: String,
    /// Opaque deep link the framework round-trips back to Music to open the
    /// result. Format: `music://{track,album,artist,playlist}/<id>`.
    pub deep_link: String,
}

impl SearchHit {
    /// The play deep-link for a track (`video_id` for a live hit, the library
    /// `TrackId` for an own-library hit — either way the id Music plays).
    pub fn track_link(id: &str) -> String {
        format!("music://track/{id}")
    }
    /// The album-page deep-link.
    pub fn album_link(id: &str) -> String {
        format!("music://album/{id}")
    }
    /// The artist-page deep-link.
    pub fn artist_link(id: &str) -> String {
        format!("music://artist/{id}")
    }
    /// The playlist-page deep-link.
    pub fn playlist_link(id: &str) -> String {
        format!("music://playlist/{id}")
    }
}

/// A parsed deep link identifying which entity a global-search hit opens. The
/// app's launch handling turns this into the real navigation / play action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeepLink {
    /// Play a track by id.
    Track(String),
    /// Open an album page.
    Album(String),
    /// Open an artist page.
    Artist(String),
    /// Open a playlist page.
    Playlist(String),
}

impl DeepLink {
    /// Parse a `music://<kind>/<id>` deep link. Rejects any other scheme (so a
    /// hostile `https://evil/…` never routes) and an empty id.
    pub fn parse(link: &str) -> Option<Self> {
        let rest = link.strip_prefix("music://")?;
        let (kind, id) = rest.split_once('/')?;
        if id.is_empty() {
            return None;
        }
        let id = id.to_string();
        match kind {
            "track" => Some(DeepLink::Track(id)),
            "album" => Some(DeepLink::Album(id)),
            "artist" => Some(DeepLink::Artist(id)),
            "playlist" => Some(DeepLink::Playlist(id)),
            _ => None,
        }
    }
}

/// Turns a query into [`SearchHit`]s by delegating to whichever
/// [`crate::backend::MusicBackend`] the build selected (offline sample by
/// default — which yields nothing — or the real rustypipe client under
/// `--features live`). The global-search framework owns ranking *across* apps;
/// this provider only contributes Music's rows, in YouTube-Music order.
pub struct MusicSearchProvider<'a> {
    backend: &'a dyn MusicBackend,
}

impl<'a> MusicSearchProvider<'a> {
    pub fn new(backend: &'a dyn MusicBackend) -> Self {
        Self { backend }
    }

    /// The descriptors this provider contributes.
    pub fn searchable_types(&self) -> Vec<SearchableType> {
        all_searchable_types()
    }

    /// Contribute hits for `query`. A failing (network / offline) search
    /// collapses to an empty contribution — a broken music search must never
    /// break OS-wide search.
    pub fn query(&self, query: &str) -> Vec<SearchHit> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let Ok(bundle) = self.backend.search_all(query) else {
            return Vec::new();
        };
        let mut hits = Vec::new();

        // The mixed "top result" leads, mirroring the app's own results page.
        if let Some(top) = &bundle.top {
            hits.push(top_hit(top));
        }

        for t in &bundle.tracks {
            hits.push(SearchHit {
                type_id: SONG_TYPE_ID,
                title: t.title.clone(),
                subtitle: song_subtitle(&t.artist, t.duration_s),
                cover_url: t.cover_url.clone(),
                thumbnail_seed: t.video_id.clone(),
                deep_link: SearchHit::track_link(&t.video_id),
            });
        }
        for a in &bundle.albums {
            hits.push(SearchHit {
                type_id: ALBUM_TYPE_ID,
                title: a.title.clone(),
                subtitle: album_subtitle(&a.artist, a.kind.label(), a.year),
                cover_url: a.cover_url.clone(),
                thumbnail_seed: a.id.clone(),
                deep_link: SearchHit::album_link(&a.id),
            });
        }
        for a in &bundle.artists {
            hits.push(SearchHit {
                type_id: ARTIST_TYPE_ID,
                title: a.name.clone(),
                subtitle: artist_subtitle(a.subscriber_count),
                cover_url: a.avatar_url.clone(),
                thumbnail_seed: a.id.clone(),
                deep_link: SearchHit::artist_link(&a.id),
            });
        }
        for p in &bundle.playlists {
            hits.push(SearchHit {
                type_id: PLAYLIST_TYPE_ID,
                title: p.title.clone(),
                subtitle: playlist_subtitle(&p.subtitle, p.track_count),
                cover_url: p.cover_url.clone(),
                thumbnail_seed: p.id.clone(),
                deep_link: SearchHit::playlist_link(&p.id),
            });
        }
        hits
    }
}

fn top_hit(top: &SearchTop) -> SearchHit {
    match top {
        SearchTop::Track(t) => SearchHit {
            type_id: SONG_TYPE_ID,
            title: t.title.clone(),
            subtitle: song_subtitle(&t.artist, t.duration_s),
            cover_url: t.cover_url.clone(),
            thumbnail_seed: t.video_id.clone(),
            deep_link: SearchHit::track_link(&t.video_id),
        },
        SearchTop::Album(a) => SearchHit {
            type_id: ALBUM_TYPE_ID,
            title: a.title.clone(),
            subtitle: album_subtitle(&a.artist, a.kind.label(), a.year),
            cover_url: a.cover_url.clone(),
            thumbnail_seed: a.id.clone(),
            deep_link: SearchHit::album_link(&a.id),
        },
        SearchTop::Artist(a) => SearchHit {
            type_id: ARTIST_TYPE_ID,
            title: a.name.clone(),
            subtitle: artist_subtitle(a.subscriber_count),
            cover_url: a.avatar_url.clone(),
            thumbnail_seed: a.id.clone(),
            deep_link: SearchHit::artist_link(&a.id),
        },
        SearchTop::Playlist(p) => SearchHit {
            type_id: PLAYLIST_TYPE_ID,
            title: p.title.clone(),
            subtitle: playlist_subtitle(&p.subtitle, p.track_count),
            cover_url: p.cover_url.clone(),
            thumbnail_seed: p.id.clone(),
            deep_link: SearchHit::playlist_link(&p.id),
        },
    }
}

fn song_subtitle(artist: &str, duration_s: u32) -> String {
    if duration_s == 0 {
        format!("Song by {artist}")
    } else {
        format!(
            "Song by {artist}, {}",
            ui::format::timecode(u64::from(duration_s))
        )
    }
}

/// An album's one secondary fact: its artist.
fn album_subtitle(artist: &str, _kind: &str, _year: u32) -> String {
    artist.to_string()
}

/// An artist's one secondary fact: how many people follow them, when known.
fn artist_subtitle(subscribers: u64) -> String {
    if subscribers == 0 {
        String::new()
    } else {
        format!("{} subscribers", ui::format::compact_count(subscribers))
    }
}

/// A playlist's one secondary fact: who made it, else how long it is.
fn playlist_subtitle(owner: &str, tracks: u64) -> String {
    match (owner.is_empty(), tracks) {
        (false, _) => owner.to_string(),
        (true, 0) => String::new(),
        (true, 1) => "1 song".into(),
        (true, n) => format!("{n} songs"),
    }
}

// ---------------------------------------------------------------------------
// Global-search framework bridge (`ui::search`).
//
// The seam above ([`SearchHit`], [`MusicSearchProvider`]) is Music's rich,
// backend-facing shape. The block below is the thin adapter that plugs it into
// the *shared* framework every Tilo app registers with — [`ui::search`] — so a
// song / album / artist / playlist appears in the one Ctrl+K palette
// [`ui::app::App`] drives, ranked on an even footing with every other app's
// hits, and activating a row routes onto Music's real play/navigate path
// (the app's launch handling). This is the reference wiring the
// [`apps/contacts`] `SearchProvider` pins, applied to Music's four types.
// ---------------------------------------------------------------------------

/// The palette group-header (kind) label for a Music result type — the plural
/// of the singular [`SearchableType::display_name`], so a Ctrl+K section reads
/// "Songs" / "Albums" / "Artists" / "Playlists".
pub fn kind_label(type_id: &str) -> &'static str {
    match type_id {
        SONG_TYPE_ID => "Songs",
        ALBUM_TYPE_ID => "Albums",
        ARTIST_TYPE_ID => "Artists",
        PLAYLIST_TYPE_ID => "Playlists",
        _ => "Music",
    }
}

/// The leading glyph a Music result type carries in the palette. The icon set
/// has no person/disc glyph, so songs and artists share the note; albums use
/// the stacked-layers glyph and playlists the list glyph — the kind header
/// disambiguates either way.
fn kind_icon(type_id: &str) -> Icon {
    match type_id {
        ALBUM_TYPE_ID => Icon::Layers,
        PLAYLIST_TYPE_ID => Icon::List,
        _ => Icon::Music,
    }
}

/// The user's **own library** — synced across their devices — as typed
/// Song/Album/Artist/Playlist search entries, the library resource's own
/// ([`crate::resource::Music`]): found from the one Ctrl+K palette and OS
/// search, each invoking the canonical action that plays the song or opens
/// the page. An empty library contributes nothing — nothing is fabricated.
///
/// It is library-scoped and synchronous: the live YouTube-Music search (which
/// does real HTTP) stays in Music's own search view, driven off-thread by the
/// [`crate::stream`] worker.
pub fn library_entries(lib: &Library) -> Vec<SearchEntry> {
    let mut entries = Vec::new();
    for track in lib.tracks.values() {
        let artist = lib.artist_name(&track.artist_id);
        let genre = lib
            .artist(&track.artist_id)
            .map_or("", |artist| artist.genre.as_str());
        let mut entry = SearchEntry::new(track.title.clone(), String::new())
            .field(artist, SearchWeight::Subtitle)
            .field(genre, SearchWeight::Metadata)
            .anchored(crate::actions::track_resource_id(&track.id))
            .grouped(kind_label(SONG_TYPE_ID))
            .subtitled(song_subtitle(artist, track.duration_s))
            .with_icon(kind_icon(SONG_TYPE_ID).name())
            .boosted(i32::try_from(track.plays).unwrap_or(i32::MAX));
        entry.row = Some(ui::tilo_resource::SearchRow::shaped(
            ui::tilo_resource::SearchRowShape::Media {
                byline: Some(artist.into()),
                duration: (track.duration_s > 0)
                    .then(|| ui::format::timecode(u64::from(track.duration_s))),
            },
        ));
        entries.push(entry);
    }
    for album in lib.albums.values() {
        let artist = lib.artist_name(&album.artist_id);
        let genre = lib
            .artist(&album.artist_id)
            .map_or("", |artist| artist.genre.as_str());
        entries.push(
            SearchEntry::new(album.title.clone(), String::new())
                .field(artist, SearchWeight::Subtitle)
                .field(genre, SearchWeight::Metadata)
                .anchored(crate::actions::album_resource_id(&album.id))
                .grouped(kind_label(ALBUM_TYPE_ID))
                .subtitled(album_subtitle(artist, "Album", album.year))
                .with_icon(kind_icon(ALBUM_TYPE_ID).name()),
        );
    }
    for artist in lib.artists.values() {
        entries.push(
            SearchEntry::new(artist.name.clone(), String::new())
                .field(artist.genre.clone(), SearchWeight::Metadata)
                .anchored(crate::actions::artist_resource_id(&artist.id))
                .grouped(kind_label(ARTIST_TYPE_ID))
                .subtitled(if artist.genre.is_empty() {
                    "Artist".to_string()
                } else {
                    artist.genre.clone()
                })
                .with_icon(kind_icon(ARTIST_TYPE_ID).name()),
        );
    }
    for playlist in lib.playlists() {
        entries.push(
            SearchEntry::new(playlist.name.clone(), String::new())
                .anchored(crate::actions::playlist_resource_id(&playlist.id))
                .grouped(kind_label(PLAYLIST_TYPE_ID))
                .subtitled(playlist_subtitle("Your library", playlist.len() as u64))
                .with_icon(kind_icon(PLAYLIST_TYPE_ID).name()),
        );
        for track_id in playlist.tracks.values() {
            let Some(track) = lib.track(track_id) else {
                continue;
            };
            let artist = lib.artist_name(&track.artist_id);
            entries.push(
                SearchEntry::new(
                    format!("{} — {}", track.title, playlist.name),
                    format!("{} {} {artist}", track.title, playlist.name),
                )
                .anchored(crate::actions::playlist_track_resource_id(
                    &playlist.id,
                    track_id,
                ))
                .grouped("Playlist tracks")
                .subtitled(format!("Open in {} by {artist}", playlist.name))
                .with_icon(kind_icon(SONG_TYPE_ID).name()),
            );
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::search::{DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult};

    /// The library's search source, as the framework builds it.
    fn source(lib: &Library) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::model::Library>(library_entries(lib))
    }

    /// The library's hits for `query` as the deep links Music routes.
    fn library_hits(lib: &Library, query: &str) -> Vec<SearchHit> {
        source(lib)
            .search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
            .into_iter()
            .filter_map(|result| library_search_result_hit(lib, &result))
            .collect()
    }

    fn library_search_result_hit(lib: &Library, result: &SearchResult) -> Option<SearchHit> {
        let anchor = result.anchor();
        if let Some(id) = anchor.strip_prefix("music.track:") {
            let track = lib.track(id)?;
            return Some(SearchHit {
                type_id: SONG_TYPE_ID,
                title: track.title.clone(),
                subtitle: song_subtitle(lib.artist_name(&track.artist_id), track.duration_s),
                cover_url: String::new(),
                thumbnail_seed: track.album_id.clone(),
                deep_link: SearchHit::track_link(&track.id),
            });
        }
        {
            if let Some(id) = anchor.strip_prefix("music.album:") {
                let album = lib.album(id)?;
                return Some(SearchHit {
                    type_id: ALBUM_TYPE_ID,
                    title: album.title.clone(),
                    subtitle: album_subtitle(
                        lib.artist_name(&album.artist_id),
                        "Album",
                        album.year,
                    ),
                    cover_url: String::new(),
                    thumbnail_seed: album.id.clone(),
                    deep_link: SearchHit::album_link(&album.id),
                });
            }
            if let Some(id) = anchor.strip_prefix("music.artist:") {
                let artist = lib.artist(id)?;
                return Some(SearchHit {
                    type_id: ARTIST_TYPE_ID,
                    title: artist.name.clone(),
                    subtitle: if artist.genre.is_empty() {
                        "Artist".into()
                    } else {
                        artist.genre.clone()
                    },
                    cover_url: String::new(),
                    thumbnail_seed: artist.id.clone(),
                    deep_link: SearchHit::artist_link(&artist.id),
                });
            }
            if let Some(id) = crate::actions::parse_playlist_resource_id(&anchor) {
                let playlist = lib.playlist(&id)?;
                return Some(SearchHit {
                    type_id: PLAYLIST_TYPE_ID,
                    title: playlist.name.clone(),
                    subtitle: playlist_subtitle("Your library", playlist.len() as u64),
                    cover_url: playlist.cover.clone(),
                    thumbnail_seed: playlist.id.clone(),
                    deep_link: SearchHit::playlist_link(&playlist.id),
                });
            }
        }
        None
    }

    use crate::backend::default_backend;
    use crate::seed;

    #[test]
    fn search_descriptors_deep_links_and_subtitles_are_stable_and_safe() {
        // Phase `descriptors_are_stable_and_complete`.
        {
            let types = all_searchable_types();
            let ids: Vec<&str> = types.iter().map(|t| t.id).collect();
            assert_eq!(
                ids,
                [
                    SONG_TYPE_ID,
                    ALBUM_TYPE_ID,
                    ARTIST_TYPE_ID,
                    PLAYLIST_TYPE_ID
                ]
            );
            assert!(types.iter().all(|t| t.has_thumbnail));
            assert!(types.iter().all(|t| !t.display_name.is_empty()));
        }

        // Phase `deep_links_round_trip`.
        {
            assert_eq!(
                DeepLink::parse(&SearchHit::track_link("dQw4w9WgXcQ")),
                Some(DeepLink::Track("dQw4w9WgXcQ".into()))
            );
            assert_eq!(
                DeepLink::parse(&SearchHit::album_link("MPREb_x")),
                Some(DeepLink::Album("MPREb_x".into()))
            );
            assert_eq!(
                DeepLink::parse(&SearchHit::artist_link("UC_abc")),
                Some(DeepLink::Artist("UC_abc".into()))
            );
            assert_eq!(
                DeepLink::parse(&SearchHit::playlist_link("PL_xyz")),
                Some(DeepLink::Playlist("PL_xyz".into()))
            );
        }

        // Phase `deep_link_rejects_hostile_and_malformed`.
        {
            assert!(DeepLink::parse("https://evil/track/x").is_none());
            assert!(DeepLink::parse("music://track/").is_none());
            assert!(DeepLink::parse("music://bogus/x").is_none());
            assert!(DeepLink::parse("music://track").is_none());
            assert!(DeepLink::parse("").is_none());
        }

        // Phase `artist_subtitle_uses_compact_subscriber_count`.
        {
            // The subscriber count in an artist subtitle is abbreviated via the
            // shared compact-count formatter (12_300 → "12.3K").
            assert_eq!(artist_subtitle(0), "");
            assert_eq!(artist_subtitle(12_300), "12.3K subscribers");
            assert_eq!(artist_subtitle(4_100_000), "4.1M subscribers");
        }
    }

    #[test]
    fn library_provider_registers_real_hits_and_fabricates_nothing() {
        // Phase `offline_provider_contributes_nothing_but_never_errors`.
        {
            // The default backend is offline: real OS search over it yields no rows
            // (the app shows the local library instead) — and crucially does not
            // fabricate any.
            let backend = default_backend();
            let provider = MusicSearchProvider::new(backend.as_ref());
            assert!(provider.query("aurora").is_empty());
            assert!(provider.query("   ").is_empty());
        }

        // Phase `library_hits_find_songs_albums_artists_and_playlists`.
        {
            let lib = seed::sample_library();

            // A song findable by its own title routes to a play deep-link.
            let some = lib.tracks.values().next().unwrap().clone();
            let hits = library_hits(&lib, &some.title);
            assert!(
                hits.iter()
                    .any(|h| h.type_id == SONG_TYPE_ID
                        && h.deep_link == SearchHit::track_link(&some.id)),
                "song hit with a play deep-link"
            );

            // An artist findable by name routes to an artist-page deep-link.
            let artist = lib.artists.values().next().unwrap().clone();
            let ahits = library_hits(&lib, &artist.name);
            assert!(
                ahits.iter().any(|h| h.type_id == ARTIST_TYPE_ID
                    && h.deep_link == SearchHit::artist_link(&artist.id)),
                "artist hit with an artist-page deep-link"
            );

            // A personal playlist findable by name routes to a playlist deep-link.
            let pl = lib.playlists().next().unwrap().clone();
            let phits = library_hits(&lib, &pl.name);
            assert!(
                phits.iter().any(|h| h.type_id == PLAYLIST_TYPE_ID
                    && h.deep_link == SearchHit::playlist_link(&pl.id)),
                "playlist hit with a playlist deep-link"
            );

            // Every hit carries a non-empty title, subtitle and deep link.
            for h in hits.iter().chain(&ahits).chain(&phits) {
                assert!(!h.title.is_empty());
                assert!(!h.subtitle.is_empty());
                assert!(DeepLink::parse(&h.deep_link).is_some());
            }
        }

        // Phase `library_hits_empty_library_is_empty`.
        {
            // An empty (default) library fabricates nothing.
            assert!(library_hits(&Library::default(), "anything").is_empty());
            assert!(library_hits(&Library::default(), "").is_empty());
        }

        // Phase `library_provider_registers_typed_rows_into_the_search_framework`.
        {
            use ui::search::SearchRegistry;
            let lib = seed::sample_library();

            // It IS a `ui::search::SearchProvider`: register it into the shared
            // registry the global Ctrl+K palette drives, alongside every other app.
            let mut registry = SearchRegistry::new();
            registry.register(Box::new(source(&lib)));
            assert_eq!(registry.provider_count(), 1);

            // A real library track surfaces as a typed "Songs" row addressing the
            // stable play effect exported by the track resource.
            let track = lib.tracks.values().next().unwrap().clone();
            let song = registry
                .query(&track.title)
                .into_iter()
                .find(|r| r.title == track.title)
                .expect("the track is findable as a framework SearchResult");
            assert_eq!(song.kind, "Songs");
            assert_eq!(song.title, track.title);
            assert_eq!(song.icon, Some(Icon::Music));
            assert_eq!(song.anchor(), crate::actions::track_resource_id(&track.id));

            // All four types section under their plural kind headers.
            let album = lib.albums.values().next().unwrap().clone();
            assert!(registry.query_grouped(&album.title).iter().any(|g| {
                g.kind == "Albums"
                    && g.results
                        .iter()
                        .any(|r| r.anchor() == crate::actions::album_resource_id(&album.id))
            }));

            let artist = lib.artists.values().next().unwrap().clone();
            assert!(registry.query_grouped(&artist.name).iter().any(|g| {
                g.kind == "Artists"
                    && g.results
                        .iter()
                        .any(|r| r.anchor() == crate::actions::artist_resource_id(&artist.id))
            }));

            let pl = lib.playlists().next().unwrap().clone();
            assert!(registry.query_grouped(&pl.name).iter().any(|g| {
                g.kind == "Playlists"
                    && g.results
                        .iter()
                        .any(|r| r.anchor() == crate::actions::playlist_resource_id(&pl.id))
            }));
        }

        // Phase `library_provider_fabricates_nothing_on_an_empty_library`.
        {
            // An empty (default) library contributes zero entries.
            assert!(library_entries(&Library::default()).is_empty());
            assert!(source(&Library::default()).search("anything").is_empty());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn indexed_provider_bounds_work_and_reaches_late_entities() {
        // Phase `indexed_provider_bounds_candidate_work_before_materialising_results`.
        {
            let mut lib = Library::default();
            lib.artists.insert(
                "artist".into(),
                crate::model::Artist {
                    id: "artist".into(),
                    name: "Bounded Artist".into(),
                    genre: "Test".into(),
                    monthly_listeners: 0,
                },
            );
            lib.albums.insert(
                "album".into(),
                crate::model::Album {
                    id: "album".into(),
                    title: "Bounded Album".into(),
                    artist_id: "artist".into(),
                    year: 2026,
                },
            );
            for n in 0..712 {
                let id = format!("track-{n}");
                lib.tracks.insert(
                    id.clone(),
                    crate::model::Track {
                        id,
                        title: format!("Song candidate {n}"),
                        album_id: "album".into(),
                        artist_id: "artist".into(),
                        track_no: 1,
                        duration_s: 1,
                        yt_id: format!("video-{n}"),
                        plays: n as u64,
                    },
                );
            }

            let provider = source(&lib);
            assert!(provider.search("absent-token").is_empty());
            let page = provider.search_page(&ui::search::SearchQuery::new("song"), None, 64, 64);
            assert!(
                page.inspected <= 64,
                "a broad query still honors the per-page inspection budget"
            );
            assert!(page.next.is_some(), "the rest stays reachable by cursor");
            assert!(
                provider.search_limited("song", usize::MAX).len() <= DEFAULT_SEARCH_RESULT_LIMIT
            );
        }

        // Phase `exact_library_lookup_reaches_entities_beyond_the_old_snapshot_cutoff`.
        {
            let mut lib = Library::default();
            lib.artists.insert(
                "artist".into(),
                crate::model::Artist {
                    id: "artist".into(),
                    name: "All Library Artist".into(),
                    genre: String::new(),
                    monthly_listeners: 0,
                },
            );
            lib.albums.insert(
                "album".into(),
                crate::model::Album {
                    id: "album".into(),
                    title: "All Library Album".into(),
                    artist_id: "artist".into(),
                    year: 2026,
                },
            );
            for n in 0..4_097 {
                let id = format!("track-{n:04}");
                lib.tracks.insert(
                    id.clone(),
                    crate::model::Track {
                        id,
                        title: if n == 4_096 {
                            "Needle Beyond Former Cutoff".into()
                        } else {
                            format!("Library track {n}")
                        },
                        album_id: "album".into(),
                        artist_id: "artist".into(),
                        track_no: 1,
                        duration_s: 1,
                        yt_id: format!("video-{n}"),
                        plays: 0,
                    },
                );
            }

            let provider = source(&lib);
            let results = provider.search("Needle Beyond Former Cutoff");
            assert!(
                results
                    .iter()
                    .any(|result| result.title == "Needle Beyond Former Cutoff"),
                "an exact title remains searchable even when it falls after the former 4,096-item cap"
            );
        }

        // Phase `distinctive_partial_phrase_finds_a_late_item_after_common_trigrams`.
        {
            let mut lib = Library::default();
            lib.artists.insert(
                "artist".into(),
                crate::model::Artist {
                    id: "artist".into(),
                    name: "Common Artist".into(),
                    genre: String::new(),
                    monthly_listeners: 0,
                },
            );
            lib.albums.insert(
                "album".into(),
                crate::model::Album {
                    id: "album".into(),
                    title: "Common Album".into(),
                    artist_id: "artist".into(),
                    year: 2026,
                },
            );
            for n in 0..4_097 {
                let id = format!("track-{n:04}");
                lib.tracks.insert(
                    id.clone(),
                    crate::model::Track {
                        id,
                        title: if n == 4_096 {
                            "Common opening distinctive moonlit signal".into()
                        } else {
                            format!("Common opening catalogue entry {n}")
                        },
                        album_id: "album".into(),
                        artist_id: "artist".into(),
                        track_no: 1,
                        duration_s: 1,
                        yt_id: format!("video-{n}"),
                        plays: 0,
                    },
                );
            }

            let provider = source(&lib);
            let results = provider.search("opening distinctive moonlit");
            assert!(
                results
                    .iter()
                    .any(|result| result.title == "Common opening distinctive moonlit signal"),
                "the rarest query trigrams intersect complete postings, so a late item survives a common opening"
            );
        }

        // Phase `provider_pages_unicode_results_without_scanning_the_whole_library`.
        {
            let mut lib = Library::default();
            lib.artists.insert(
                "artist".into(),
                crate::model::Artist {
                    id: "artist".into(),
                    name: "Mañana".into(),
                    genre: "Électronica".into(),
                    monthly_listeners: 0,
                },
            );
            lib.albums.insert(
                "album".into(),
                crate::model::Album {
                    id: "album".into(),
                    title: "Café Sessions".into(),
                    artist_id: "artist".into(),
                    year: 2026,
                },
            );
            for number in 0..40 {
                let id = format!("track-{number:03}");
                lib.tracks.insert(
                    id.clone(),
                    crate::model::Track {
                        id,
                        title: if number == 39 {
                            "Café Paged Finale 🎧".into()
                        } else {
                            format!("Café paged song {number:03}")
                        },
                        album_id: "album".into(),
                        artist_id: "artist".into(),
                        track_no: 1,
                        duration_s: 1,
                        yt_id: format!("video-{number}"),
                        plays: 0,
                    },
                );
            }
            let provider = source(&lib);
            let first = provider.search_page(&ui::search::SearchQuery::new("CAFÉ"), None, 7, 7);
            assert_eq!(first.inspected, 7);
            assert_eq!(first.results.len(), 7);
            let next = first.next.as_ref().expect("more matching rows remain");
            let second =
                provider.search_page(&ui::search::SearchQuery::new("CAFÉ"), Some(next), 7, 7);
            assert_eq!(second.inspected, 7);
            assert!(
                second
                    .results
                    .iter()
                    .all(|result| crate::actions::anchor_effect(&result.anchor()).is_some())
            );

            let all = provider.search("PAGED FINALE 🎧");
            assert!(
                all.iter()
                    .any(|result| result.title == "Café Paged Finale 🎧")
            );
        }

        // Phase `provider_indexes_playlist_memberships_with_safe_canonical_open_actions`.
        {
            let lib = seed::sample_library();
            let playlist = lib.playlists().next().expect("seed playlist");
            let track_ids = playlist.track_ids();
            let track_id = track_ids.first().expect("seed membership");
            let track = lib.track(track_id).expect("seed track");
            let provider = source(&lib);
            let anchor = crate::actions::playlist_track_resource_id(&playlist.id, track_id);
            assert!(
                provider.search(&track.title).iter().any(|result| {
                    result.anchor() == anchor && result.kind == "Playlist tracks"
                })
            );
            assert_eq!(
                crate::actions::anchor_effect(&anchor),
                Some(crate::actions::MusicEffect::Open {
                    place: crate::actions::Place::Membership {
                        playlist: playlist.id.clone(),
                        track: track_id.clone(),
                    }
                }),
                "opening the hit shows the membership"
            );
        }
    }
}
