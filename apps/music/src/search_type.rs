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
//! * [`library_hits`] — contributes the user's **own library** (synced across
//!   their devices) to OS search, so a saved album or a personal playlist is
//!   findable even on the hermetic/offline build.
//!
//! Descriptor registration is a one-liner
//! (`for t in music::searchable_types() { registry.register(t) }` plus a
//! provider query with no reach into app internals. Search activation itself
//! goes through the shared operation broker and Music's exported resource tree.

use tilo_actions::ActionAddress;
use ui::icons::Icon;
use ui::search::{
    CatalogSearchProvider, DEFAULT_SEARCH_RESULT_LIMIT, SearchCatalog, SearchProvider,
    SearchResult, SearchRow,
};

use crate::backend::{MusicBackend, SearchTop};
use crate::model::Library;

/// A query never evaluates more than this many indexed library entities. The
/// palette has a much smaller visible result limit; the extra headroom keeps
/// common short-prefix searches useful without turning a no-match query into a
/// linear scan of the local catalogue.
const SEARCH_CANDIDATE_LIMIT: usize = 512;

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
    LibrarySearchProvider::new(lib)
        .search_limited(query, DEFAULT_SEARCH_RESULT_LIMIT)
        .into_iter()
        .filter_map(|result| library_search_result_hit(lib, &result))
        .collect()
}

fn library_search_result_hit(lib: &Library, result: &SearchResult) -> Option<SearchHit> {
    let address = result.action_address()?;
    if address.action == "play" {
        let id = address.resource.strip_prefix("music.track:")?;
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
    if address.action == "open" {
        if let Some(id) = address.resource.strip_prefix("music.album:") {
            let album = lib.album(id)?;
            return Some(SearchHit {
                type_id: ALBUM_TYPE_ID,
                title: album.title.clone(),
                subtitle: album_subtitle(lib.artist_name(&album.artist_id), "Album", album.year),
                cover_url: String::new(),
                thumbnail_seed: album.id.clone(),
                deep_link: SearchHit::album_link(&album.id),
            });
        }
        if let Some(id) = address.resource.strip_prefix("music.artist:") {
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
        if let Some(id) = address.resource.strip_prefix("music.playlist:") {
            let playlist = lib.playlist(id)?;
            return Some(SearchHit {
                type_id: PLAYLIST_TYPE_ID,
                title: playlist.name.clone(),
                subtitle: playlist_subtitle("Your library", playlist.track_ids.len() as u64),
                cover_url: playlist.cover.clone(),
                thumbnail_seed: playlist.id.clone(),
                deep_link: SearchHit::playlist_link(&playlist.id),
            });
        }
    }
    None
}

/// A song's one secondary fact: its artist. The kind is the group header and
/// the duration has its own column in a media row.
fn song_subtitle(artist: &str, _duration_s: u32) -> String {
    artist.to_string()
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

/// The [`ui::search::SearchProvider`] Music registers into the global Ctrl+K
/// palette. It contributes the user's **own library** — synced across their
/// devices — as typed Song/Album/Artist/Playlist rows.
///
/// It is deliberately library-scoped and synchronous: a [`SearchProvider`] is
/// queried on the UI thread, so it must never block on the network. The live
/// YouTube-Music catalogue (which does real HTTP) answers through
/// [`crate::live_search::LiveCatalogProvider`], driven off-thread by the
/// [`crate::stream`] worker.
///
/// Holds a cheap clone of a prebuilt shared catalog. [`MusicApp`](crate::app::MusicApp)
/// retains the catalog and refreshes it only when its library revision changes,
/// so merely opening Ctrl+K never rebuilds or reindexes the whole corpus. An
/// empty library contributes zero rows — nothing is fabricated.
#[derive(Clone)]
pub struct LibrarySearchProvider {
    catalog: SearchCatalog,
}

impl LibrarySearchProvider {
    /// Snapshot the current library into a provider.
    #[allow(
        clippy::too_many_lines,
        reason = "the complete four-kind catalog is intentionally constructed together so canonical resource/action identities cannot drift between search kinds"
    )]
    pub fn new(lib: &Library) -> Self {
        let catalog = SearchCatalog::new("Music");
        for track in lib.tracks.values() {
            let artist = lib.artist_name(&track.artist_id);
            let genre = lib
                .artist(&track.artist_id)
                .map_or("", |artist| artist.genre.as_str());
            let mut row = SearchRow::media(&track.title).byline(artist);
            if track.duration_s > 0 {
                row = row.duration(ui::format::timecode(u64::from(track.duration_s)));
            }
            let result = SearchResult::for_action(
                kind_label(SONG_TYPE_ID),
                &track.title,
                crate::actions::play_track_address(&track.id),
            )
            .with_icon(kind_icon(SONG_TYPE_ID))
            .with_subtitle(song_subtitle(artist, track.duration_s))
            .with_row(row)
            .with_score(track.plays.min(i32::MAX as u64) as i32);
            // The resource id and canonical action address are deliberately
            // identical at the namespace boundary. SearchCatalog rejects any
            // mismatch, preserving the authority door for every result.
            let _ = catalog.upsert(
                crate::actions::track_resource_id(&track.id),
                format!("{} {artist} {genre}", track.title),
                result,
            );
        }
        for album in lib.albums.values() {
            let artist = lib.artist_name(&album.artist_id);
            let genre = lib
                .artist(&album.artist_id)
                .map_or("", |artist| artist.genre.as_str());
            let result = SearchResult::for_action(
                kind_label(ALBUM_TYPE_ID),
                &album.title,
                crate::actions::open_album_address(&album.id),
            )
            .with_icon(kind_icon(ALBUM_TYPE_ID))
            .with_subtitle(album_subtitle(artist, "Album", album.year));
            let _ = catalog.upsert(
                crate::actions::album_resource_id(&album.id),
                format!("{} {artist} {genre}", album.title),
                result,
            );
        }
        for artist in lib.artists.values() {
            let result = SearchResult::for_action(
                kind_label(ARTIST_TYPE_ID),
                &artist.name,
                crate::actions::open_artist_address(&artist.id),
            )
            .with_icon(kind_icon(ARTIST_TYPE_ID))
            .with_subtitle(artist.genre.clone());
            let _ = catalog.upsert(
                crate::actions::artist_resource_id(&artist.id),
                format!("{} {}", artist.name, artist.genre),
                result,
            );
        }
        for playlist in &lib.playlists {
            let result = SearchResult::for_action(
                kind_label(PLAYLIST_TYPE_ID),
                &playlist.name,
                crate::actions::open_playlist_address(&playlist.id),
            )
            .with_icon(kind_icon(PLAYLIST_TYPE_ID))
            .with_subtitle(playlist_subtitle("", playlist.track_ids.len() as u64));
            let _ = catalog.upsert(
                crate::actions::playlist_resource_id(&playlist.id),
                &playlist.name,
                result,
            );
            for track_id in &playlist.track_ids {
                let Some(track) = lib.track(track_id) else {
                    continue;
                };
                let resource_id =
                    crate::actions::playlist_track_resource_id(&playlist.id, track_id);
                let result = SearchResult::for_action(
                    "Playlist tracks",
                    &track.title,
                    crate::actions::open_playlist_membership_address(&playlist.id, track_id),
                )
                .with_icon(kind_icon(SONG_TYPE_ID))
                .with_subtitle(format!("In {}", playlist.name));
                let _ = catalog.upsert(
                    resource_id,
                    format!(
                        "{} {} {}",
                        track.title,
                        playlist.name,
                        lib.artist_name(&track.artist_id)
                    ),
                    result,
                );
            }
        }
        Self { catalog }
    }

    #[cfg(test)]
    fn candidate_count_for_query(&self, query: &str) -> usize {
        self.catalog
            .query_page(query, None, SEARCH_CANDIDATE_LIMIT, 1)
            .inspected
    }
}

impl CatalogSearchProvider for LibrarySearchProvider {
    fn provider_name(&self) -> &'static str {
        // Default group label / diagnostic name; individual rows carry their own
        // per-type kind ("Songs", "Albums", …) so the overlay sections them.
        "Music"
    }

    fn catalog(&self) -> Option<SearchCatalog> {
        Some(self.catalog.clone())
    }

    fn limited_work_budget(&self, _limit: usize) -> usize {
        SEARCH_CANDIDATE_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::default_backend;
    use crate::seed;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
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

        // Phase `library_hits_empty_query_and_empty_library_are_empty`.
        {
            let lib = seed::sample_library();
            assert!(library_hits(&lib, "   ").is_empty());
            // An empty (default) library fabricates nothing.
            assert!(library_hits(&Library::default(), "anything").is_empty());
        }

        // Phase `library_provider_registers_typed_rows_into_the_search_framework`.
        {
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

        // Phase `library_provider_fabricates_nothing_on_empty_library_or_query`.
        {
            // An empty (default) library contributes zero rows for any query.
            let empty = LibrarySearchProvider::new(&Library::default());
            assert!(empty.search("anything").is_empty());
            // A seeded library still browses nothing on a blank query (no catalog
            // dump into the palette).
            let seeded = LibrarySearchProvider::new(&seed::sample_library());
            assert!(seeded.search("   ").is_empty());
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
            for n in 0..(SEARCH_CANDIDATE_LIMIT + 200) {
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

            let provider = LibrarySearchProvider::new(&lib);
            assert!(
                provider.candidate_count_for_query("absent-token") <= SEARCH_CANDIDATE_LIMIT,
                "a no-match query consults an index posting, never every library entity"
            );
            assert_eq!(provider.candidate_count_for_query("absent-token"), 0);
            assert!(
                provider.candidate_count_for_query("song") <= SEARCH_CANDIDATE_LIMIT,
                "a broad query still honors the per-page inspection budget"
            );
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

            let provider = LibrarySearchProvider::new(&lib);
            let results = provider.search("Needle Beyond Former Cutoff");
            assert!(
                results
                    .iter()
                    .any(|result| result.title == "Needle Beyond Former Cutoff"),
                "an exact title remains searchable even when it falls after the former 4,096-item cap"
            );
            assert_eq!(
                provider.candidate_count_for_query("Needle Beyond Former Cutoff"),
                1
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

            let provider = LibrarySearchProvider::new(&lib);
            let results = provider.search("opening distinctive moonlit");
            assert!(
                results
                    .iter()
                    .any(|result| result.title == "Common opening distinctive moonlit signal"),
                "the rarest query trigrams intersect complete postings, so a late item survives a common opening"
            );
            assert_eq!(
                provider.candidate_count_for_query("opening distinctive moonlit"),
                1
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
            let provider = LibrarySearchProvider::new(&lib);
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
                    .all(|result| result.action_address().is_some())
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
            let playlist = lib.playlists.first().expect("seed playlist");
            let track_id = playlist.track_ids.first().expect("seed membership");
            let track = lib.track(track_id).expect("seed track");
            let provider = LibrarySearchProvider::new(&lib);
            let address = crate::actions::open_playlist_membership_address(&playlist.id, track_id);
            assert!(provider.search(&track.title).iter().any(|result| {
                result.action_address().as_ref() == Some(&address)
                    && result.kind == "Playlist tracks"
            }));
        }
    }
}
