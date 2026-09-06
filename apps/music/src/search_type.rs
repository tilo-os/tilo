//! Search-type seam — registering Music's result types with qOS's coming
//! **global-search** framework.
//!
//! Music must *not* build a bespoke OS-wide search. Instead it exposes clean
//! descriptors the global-search framework can register, so a song / album /
//! artist / playlist shows up in the OS search palette with a cover, a title
//! and a subtitle — and activating a hit resolves a canonical resource action
//! that plays the song or opens the album/artist/playlist page.
//!
//! The global-search framework doesn't exist in-tree yet, so this module
//! defines the *provider side* of the contract Music can already satisfy — the
//! exact shape [`apps/tube`] pins for its videos/channels:
//!
//! * [`SearchableType`] — the static descriptor (stable id, display name,
//!   whether hits carry a cover). One per app-provided result type.
//! * [`SearchHit`] — one result row: title, subtitle, a real cover URL (live)
//!   or a procedural seed (offline), plus the deep link that opens it.
//! * [`MusicSearchProvider`] — turns a query into [`SearchHit`]s by delegating
//!   to whichever [`crate::backend::MusicBackend`] the build selected (offline
//!   sample by default, real `YouTube` Music under `--features live`).
//! * [`library_hits`] — contributes the user's **own library** (synced across
//!   their devices) to OS search, so a saved album or a personal playlist is
//!   findable even on the hermetic/offline build.
//!
//! When the framework arrives, the integration is a one-liner
//! (`for t in music::searchable_types() { registry.register(t) }` plus a
//! provider query with no reach into app internals. Search activation itself
//! goes through the shared operation broker and Music's exported resource tree.

use qos_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    DEFAULT_SEARCH_INDEX_LIMIT, DEFAULT_SEARCH_RESULT_LIMIT, SearchProvider, SearchResult,
};

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
/// app's router ([`crate::MusicApp::open_deep_link`]) turns this into the real
/// navigation / play action.
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

    /// The [`SearchableType`] id this deep link belongs to.
    pub fn type_id(&self) -> &'static str {
        match self {
            DeepLink::Track(_) => SONG_TYPE_ID,
            DeepLink::Album(_) => ALBUM_TYPE_ID,
            DeepLink::Artist(_) => ARTIST_TYPE_ID,
            DeepLink::Playlist(_) => PLAYLIST_TYPE_ID,
        }
    }

    /// The canonical resource/action address for this entity. Search uses this
    /// address directly; the deep-link representation remains only for the
    /// app's external-link compatibility surface.
    pub fn action_address(&self) -> ActionAddress {
        match self {
            DeepLink::Track(id) => crate::actions::play_track_address(id),
            DeepLink::Album(id) => crate::actions::open_album_address(id),
            DeepLink::Artist(id) => crate::actions::open_artist_address(id),
            DeepLink::Playlist(id) => crate::actions::open_playlist_address(id),
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

/// Contribute the user's **own library** — synced across their devices — to OS
/// search. Runs against the in-memory [`Library`] the app already loads from
/// the CRDT, so a saved album or a personal playlist is findable OS-wide even
/// on the hermetic/offline build (where the live backend yields nothing).
/// Empty query yields nothing; nothing here is fabricated — an empty library
/// contributes zero rows.
pub fn library_hits(lib: &Library, query: &str) -> Vec<SearchHit> {
    library_hits_limited(lib, query, DEFAULT_SEARCH_RESULT_LIMIT)
}

fn library_hits_limited(lib: &Library, query: &str, limit: usize) -> Vec<SearchHit> {
    let q = query.trim();
    let limit = limit.min(DEFAULT_SEARCH_RESULT_LIMIT);
    if q.is_empty() || limit == 0 {
        return Vec::new();
    }
    let needle = q.to_lowercase();
    let artist_genre = |id: &str| {
        lib.artists
            .get(id)
            .map(|artist| artist.genre.to_lowercase())
            .unwrap_or_default()
    };
    let mut top_tracks = std::collections::BinaryHeap::new();
    for track in lib.tracks.values().filter(|track| {
        track.title.to_lowercase().contains(&needle)
            || lib
                .artist_name(&track.artist_id)
                .to_lowercase()
                .contains(&needle)
            || artist_genre(&track.artist_id).contains(&needle)
    }) {
        top_tracks.push(std::cmp::Reverse((
            track.plays,
            std::cmp::Reverse(track.id.as_str()),
        )));
        if top_tracks.len() > limit {
            top_tracks.pop();
        }
    }
    // Library::search ranks tracks by play count. Retain its exact top-k in a
    // bounded heap while scanning the bounded snapshot, then restore the
    // stable popularity/id order for presentation.
    let mut tracks = top_tracks
        .into_iter()
        .filter_map(|std::cmp::Reverse((_, std::cmp::Reverse(id)))| lib.track(id))
        .collect::<Vec<_>>();
    tracks.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.id.cmp(&b.id)));
    let mut hits = Vec::with_capacity(limit.min(32));
    for t in tracks {
        hits.push(SearchHit {
            type_id: SONG_TYPE_ID,
            title: t.title.clone(),
            subtitle: song_subtitle(lib.artist_name(&t.artist_id), t.duration_s),
            cover_url: String::new(),
            thumbnail_seed: t.album_id.clone(),
            deep_link: SearchHit::track_link(&t.id),
        });
    }
    if hits.len() == limit {
        return hits;
    }
    for a in lib.albums.values().filter(|album| {
        album.title.to_lowercase().contains(&needle)
            || artist_genre(&album.artist_id).contains(&needle)
    }) {
        hits.push(SearchHit {
            type_id: ALBUM_TYPE_ID,
            title: a.title.clone(),
            subtitle: album_subtitle(lib.artist_name(&a.artist_id), "Album", a.year),
            cover_url: String::new(),
            thumbnail_seed: a.id.clone(),
            deep_link: SearchHit::album_link(&a.id),
        });
        if hits.len() == limit {
            return hits;
        }
    }
    for a in lib.artists.values().filter(|artist| {
        artist.name.to_lowercase().contains(&needle)
            || artist.genre.to_lowercase().contains(&needle)
    }) {
        hits.push(SearchHit {
            type_id: ARTIST_TYPE_ID,
            title: a.name.clone(),
            subtitle: if a.genre.is_empty() {
                "Artist".into()
            } else {
                a.genre.clone()
            },
            cover_url: String::new(),
            thumbnail_seed: a.id.clone(),
            deep_link: SearchHit::artist_link(&a.id),
        });
        if hits.len() == limit {
            return hits;
        }
    }
    append_matching_playlists(lib, q, limit, &mut hits);
    hits
}

fn append_matching_playlists(lib: &Library, query: &str, limit: usize, hits: &mut Vec<SearchHit>) {
    // Library::search does not include playlists, but the user's own synced
    // playlists should still be discoverable OS-wide.
    let query = query.to_lowercase();
    for playlist in &lib.playlists {
        if playlist.name.to_lowercase().contains(&query) {
            hits.push(SearchHit {
                type_id: PLAYLIST_TYPE_ID,
                title: playlist.name.clone(),
                subtitle: playlist_subtitle("Your library", playlist.track_ids.len() as u64),
                cover_url: playlist.cover.clone(),
                thumbnail_seed: playlist.id.clone(),
                deep_link: SearchHit::playlist_link(&playlist.id),
            });
            if hits.len() == limit {
                break;
            }
        }
    }
}

fn song_subtitle(artist: &str, duration_s: u32) -> String {
    if duration_s == 0 {
        format!("Song · {artist}")
    } else {
        format!(
            "Song · {artist} · {}",
            ui::format::timecode(u64::from(duration_s))
        )
    }
}

fn album_subtitle(artist: &str, kind: &str, year: u32) -> String {
    if year == 0 {
        format!("{kind} · {artist}")
    } else {
        format!("{kind} · {artist} · {year}")
    }
}

fn artist_subtitle(subscribers: u64) -> String {
    if subscribers == 0 {
        "Artist".into()
    } else {
        format!(
            "Artist · {} subscribers",
            ui::format::compact_count(subscribers)
        )
    }
}

fn playlist_subtitle(owner: &str, tracks: u64) -> String {
    match (owner.is_empty(), tracks) {
        (true, 0) => "Playlist".into(),
        (true, n) => format!("Playlist · {n} songs"),
        (false, 0) => format!("Playlist · {owner}"),
        (false, n) => format!("Playlist · {owner} · {n} songs"),
    }
}

// ---------------------------------------------------------------------------
// Global-search framework bridge (`ui::search`).
//
// The seam above ([`SearchHit`], [`MusicSearchProvider`]) is Music's rich,
// backend-facing shape. The block below is the thin adapter that plugs it into
// the *shared* framework every qOS app registers with — [`ui::search`] — so a
// song / album / artist / playlist appears in the one Ctrl+K palette
// [`ui::app::App`] drives, ranked on an even footing with every other app's
// hits, and activating a row routes onto Music's real play/navigate path
// (see [`crate::MusicApp::open_deep_link`]). This is the reference wiring the
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

impl SearchHit {
    /// Adapt a Music hit onto the framework's typed [`SearchResult`], addressing
    /// the exact play/open operation exported by Music's live resource tree.
    fn into_search_result(self) -> Option<SearchResult> {
        let target = DeepLink::parse(&self.deep_link)?;
        let mut r = SearchResult::for_action(
            kind_label(self.type_id),
            self.title,
            target.action_address(),
        )
        .with_icon(kind_icon(self.type_id));
        if !self.subtitle.is_empty() {
            r = r.with_subtitle(self.subtitle);
        }
        Some(r)
    }
}

/// The [`ui::search::SearchProvider`] Music registers into the global Ctrl+K
/// palette. It contributes the user's **own library** — synced across their
/// devices — as typed Song/Album/Artist/Playlist rows.
///
/// It is deliberately library-scoped and synchronous: a [`SearchProvider`] is
/// queried on the UI thread, so it must never block on the network. The live
/// YouTube-Music search (which does real HTTP) stays in Music's own search
/// view, driven off-thread by the [`crate::stream`] worker. The deep links this
/// provider emits and the live view emits are the same shape, so a future
/// shell-level Ctrl+K can register this provider directly and every activation
/// still lands on the one router.
///
/// Holds an owned snapshot of the [`Library`] (cheap to rebuild whenever the
/// search surface opens), mirroring [`apps/contacts`]' provider. An empty
/// library contributes zero rows — nothing is fabricated.
pub struct LibrarySearchProvider {
    lib: Library,
}

impl LibrarySearchProvider {
    /// Snapshot the current library into a provider.
    pub fn new(lib: &Library) -> Self {
        let mut bounded = Library::default();
        for (id, track) in lib.tracks.iter().take(DEFAULT_SEARCH_INDEX_LIMIT) {
            bounded.tracks.insert(id.clone(), track.clone());
        }
        for (id, album) in lib.albums.iter().take(DEFAULT_SEARCH_INDEX_LIMIT) {
            bounded.albums.insert(id.clone(), album.clone());
        }
        for (id, artist) in lib.artists.iter().take(DEFAULT_SEARCH_INDEX_LIMIT) {
            bounded.artists.insert(id.clone(), artist.clone());
        }
        bounded.playlists.extend(
            lib.playlists
                .iter()
                .take(DEFAULT_SEARCH_INDEX_LIMIT)
                .cloned(),
        );
        Self { lib: bounded }
    }
}

impl SearchProvider for LibrarySearchProvider {
    fn name(&self) -> &'static str {
        // Default group label / diagnostic name; individual rows carry their own
        // per-type kind ("Songs", "Albums", …) so the overlay sections them.
        "Music"
    }

    fn search(&self, query: &str) -> Vec<SearchResult> {
        self.search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
    }

    fn search_limited(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        library_hits_limited(&self.lib, query, limit)
            .into_iter()
            .filter_map(SearchHit::into_search_result)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::default_backend;
    use crate::seed;

    #[test]
    fn descriptors_are_stable_and_complete() {
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

    #[test]
    fn deep_links_round_trip() {
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

    #[test]
    fn deep_link_rejects_hostile_and_malformed() {
        assert!(DeepLink::parse("https://evil/track/x").is_none());
        assert!(DeepLink::parse("music://track/").is_none());
        assert!(DeepLink::parse("music://bogus/x").is_none());
        assert!(DeepLink::parse("music://track").is_none());
        assert!(DeepLink::parse("").is_none());
    }

    #[test]
    fn offline_provider_contributes_nothing_but_never_errors() {
        // The default backend is offline: real OS search over it yields no rows
        // (the app shows the local library instead) — and crucially does not
        // fabricate any.
        let backend = default_backend();
        let provider = MusicSearchProvider::new(backend.as_ref());
        assert!(provider.query("aurora").is_empty());
        assert!(provider.query("   ").is_empty());
    }

    #[test]
    fn library_hits_find_songs_albums_artists_and_playlists() {
        let lib = seed::sample_library();

        // A song findable by its own title routes to a play deep-link.
        let some = lib.tracks.values().next().unwrap().clone();
        let hits = library_hits(&lib, &some.title);
        assert!(
            hits.iter().any(
                |h| h.type_id == SONG_TYPE_ID && h.deep_link == SearchHit::track_link(&some.id)
            ),
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
        let pl = lib.playlists.first().unwrap().clone();
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

    #[test]
    fn library_hits_empty_query_and_empty_library_are_empty() {
        let lib = seed::sample_library();
        assert!(library_hits(&lib, "   ").is_empty());
        // An empty (default) library fabricates nothing.
        assert!(library_hits(&Library::default(), "anything").is_empty());
    }

    #[test]
    fn artist_subtitle_uses_compact_subscriber_count() {
        // The subscriber count in an artist subtitle is abbreviated via the
        // shared compact-count formatter (12_300 → "12.3K").
        assert_eq!(artist_subtitle(0), "Artist");
        assert_eq!(artist_subtitle(12_300), "Artist · 12.3K subscribers");
        assert_eq!(artist_subtitle(4_100_000), "Artist · 4.1M subscribers");
    }

    #[test]
    fn library_provider_registers_typed_rows_into_the_search_framework() {
        use ui::search::SearchRegistry;
        let lib = seed::sample_library();

        // It IS a `ui::search::SearchProvider`: register it into the shared
        // registry the global Ctrl+K palette drives, alongside every other app.
        let mut registry = SearchRegistry::new();
        registry.register(Box::new(LibrarySearchProvider::new(&lib)));
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
        assert_eq!(
            song.action_address(),
            Some(crate::actions::play_track_address(&track.id))
        );

        // All four types section under their plural kind headers.
        let album = lib.albums.values().next().unwrap().clone();
        assert!(registry.query_grouped(&album.title).iter().any(|g| {
            g.kind == "Albums"
                && g.results.iter().any(|r| {
                    r.action_address() == Some(crate::actions::open_album_address(&album.id))
                })
        }));

        let artist = lib.artists.values().next().unwrap().clone();
        assert!(registry.query_grouped(&artist.name).iter().any(|g| {
            g.kind == "Artists"
                && g.results.iter().any(|r| {
                    r.action_address() == Some(crate::actions::open_artist_address(&artist.id))
                })
        }));

        let pl = lib.playlists.first().unwrap().clone();
        assert!(registry.query_grouped(&pl.name).iter().any(|g| {
            g.kind == "Playlists"
                && g.results.iter().any(|r| {
                    r.action_address() == Some(crate::actions::open_playlist_address(&pl.id))
                })
        }));
    }

    #[test]
    fn library_provider_fabricates_nothing_on_empty_library_or_query() {
        // An empty (default) library contributes zero rows for any query.
        let empty = LibrarySearchProvider::new(&Library::default());
        assert!(empty.search("anything").is_empty());
        // A seeded library still browses nothing on a blank query (no catalog
        // dump into the palette).
        let seeded = LibrarySearchProvider::new(&seed::sample_library());
        assert!(seeded.search("   ").is_empty());
    }
}
