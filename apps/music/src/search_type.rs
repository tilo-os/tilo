//! Music's search results: the descriptors and hits the one Ctrl+K palette shows, and
//! the library's own entries as a synchronous source.

use ui::icons::Icon;
use ui::tilo_resource::{SearchEntry, SearchWeight};

use crate::backend::{MusicBackend, SearchTop};
use crate::model::Library;

/// Ids the framework dedupes providers and persists per-type preferences by.
pub const SONG_TYPE_ID: &str = "music.song";
pub const ALBUM_TYPE_ID: &str = "music.album";
pub const ARTIST_TYPE_ID: &str = "music.artist";
pub const PLAYLIST_TYPE_ID: &str = "music.playlist";

/// A result type Music adds to global search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchableType {
    pub id: &'static str,
    pub display_name: &'static str,
    /// Whether hits carry a cover, so the palette reserves image space; artists get a round
    /// avatar.
    pub has_thumbnail: bool,
}

pub fn song_searchable_type() -> SearchableType {
    SearchableType {
        id: SONG_TYPE_ID,
        display_name: "Song",
        has_thumbnail: true,
    }
}

pub fn album_searchable_type() -> SearchableType {
    SearchableType {
        id: ALBUM_TYPE_ID,
        display_name: "Album",
        has_thumbnail: true,
    }
}

pub fn artist_searchable_type() -> SearchableType {
    SearchableType {
        id: ARTIST_TYPE_ID,
        display_name: "Artist",
        has_thumbnail: true,
    }
}

pub fn playlist_searchable_type() -> SearchableType {
    SearchableType {
        id: PLAYLIST_TYPE_ID,
        display_name: "Playlist",
        has_thumbnail: true,
    }
}

/// Every result type, in the order the palette sections them.
pub fn all_searchable_types() -> Vec<SearchableType> {
    vec![
        song_searchable_type(),
        album_searchable_type(),
        artist_searchable_type(),
        playlist_searchable_type(),
    ]
}

/// One result row Music contributes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    pub type_id: &'static str,
    pub title: String,
    /// A second line that tells similar hits apart.
    pub subtitle: String,
    /// A cover URL when the catalogue has one; when it is empty the palette draws the
    /// procedural tile keyed by [`Self::thumbnail_seed`].
    pub cover_url: String,
    /// The seed of Music's cover gradient, used when [`Self::cover_url`] is empty.
    pub thumbnail_seed: String,
    /// The link the framework hands back: `music://{track,album,artist,playlist}/<id>`.
    pub deep_link: String,
}

impl SearchHit {
    /// A track's link: the catalogue video id, or the library id.
    pub fn track_link(id: &str) -> String {
        format!("music://track/{id}")
    }
    pub fn album_link(id: &str) -> String {
        format!("music://album/{id}")
    }
    pub fn artist_link(id: &str) -> String {
        format!("music://artist/{id}")
    }
    pub fn playlist_link(id: &str) -> String {
        format!("music://playlist/{id}")
    }
}

/// A parsed deep link: what a hit opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeepLink {
    Track(String),
    Album(String),
    Artist(String),
    Playlist(String),
}

impl DeepLink {
    /// Parse a `music://<kind>/<id>` link; any other scheme, and an empty id, is rejected.
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

/// Turns a query into hits through the backend the build selected; the framework ranks
/// across apps, this provider only adds Music's rows.
pub struct MusicSearchProvider<'a> {
    backend: &'a dyn MusicBackend,
}

impl<'a> MusicSearchProvider<'a> {
    pub fn new(backend: &'a dyn MusicBackend) -> Self {
        Self { backend }
    }

    pub fn searchable_types(&self) -> Vec<SearchableType> {
        all_searchable_types()
    }

    /// Hits for `query`; a failing search contributes nothing rather than break OS search.
    pub fn query(&self, query: &str) -> Vec<SearchHit> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        let Ok(bundle) = self.backend.search_all(query) else {
            return Vec::new();
        };
        let mut hits = Vec::new();

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

fn album_subtitle(artist: &str, _kind: &str, _year: u32) -> String {
    artist.to_string()
}

fn artist_subtitle(subscribers: u64) -> String {
    if subscribers == 0 {
        String::new()
    } else {
        format!("{} subscribers", ui::format::compact_count(subscribers))
    }
}

fn playlist_subtitle(owner: &str, tracks: u64) -> String {
    match (owner.is_empty(), tracks) {
        (false, _) => owner.to_string(),
        (true, 0) => String::new(),
        (true, 1) => "1 song".into(),
        (true, n) => format!("{n} songs"),
    }
}

/// The plural of a type's name, for its palette section header.
pub fn kind_label(type_id: &str) -> &'static str {
    match type_id {
        SONG_TYPE_ID => "Songs",
        ALBUM_TYPE_ID => "Albums",
        ARTIST_TYPE_ID => "Artists",
        PLAYLIST_TYPE_ID => "Playlists",
        _ => "Music",
    }
}

/// A type's glyph in the palette: songs and artists share the note, albums the layers,
/// playlists the list.
fn kind_icon(type_id: &str) -> Icon {
    match type_id {
        ALBUM_TYPE_ID => Icon::Layers,
        PLAYLIST_TYPE_ID => Icon::List,
        _ => Icon::Music,
    }
}

/// The library's own entries, synchronous: catalogue search stays in Music's view.
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

    fn source(lib: &Library) -> impl SearchProvider + use<> {
        ui::framework::testing::search_source::<crate::model::Library>(library_entries(lib))
    }

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

        {
            assert!(DeepLink::parse("https://evil/track/x").is_none());
            assert!(DeepLink::parse("music://track/").is_none());
            assert!(DeepLink::parse("music://bogus/x").is_none());
            assert!(DeepLink::parse("music://track").is_none());
            assert!(DeepLink::parse("").is_none());
        }

        {
            assert_eq!(artist_subtitle(0), "");
            assert_eq!(artist_subtitle(12_300), "12.3K subscribers");
            assert_eq!(artist_subtitle(4_100_000), "4.1M subscribers");
        }
    }

    #[test]
    fn library_provider_registers_real_hits_and_fabricates_nothing() {
        {
            let backend = default_backend();
            let provider = MusicSearchProvider::new(backend.as_ref());
            assert!(provider.query("aurora").is_empty());
            assert!(provider.query("   ").is_empty());
        }

        {
            let lib = seed::sample_library();

            let some = lib.tracks.values().next().unwrap().clone();
            let hits = library_hits(&lib, &some.title);
            assert!(
                hits.iter()
                    .any(|h| h.type_id == SONG_TYPE_ID
                        && h.deep_link == SearchHit::track_link(&some.id)),
                "song hit with a play deep-link"
            );

            let artist = lib.artists.values().next().unwrap().clone();
            let ahits = library_hits(&lib, &artist.name);
            assert!(
                ahits.iter().any(|h| h.type_id == ARTIST_TYPE_ID
                    && h.deep_link == SearchHit::artist_link(&artist.id)),
                "artist hit with an artist-page deep-link"
            );

            let pl = lib.playlists().next().unwrap().clone();
            let phits = library_hits(&lib, &pl.name);
            assert!(
                phits.iter().any(|h| h.type_id == PLAYLIST_TYPE_ID
                    && h.deep_link == SearchHit::playlist_link(&pl.id)),
                "playlist hit with a playlist deep-link"
            );

            for h in hits.iter().chain(&ahits).chain(&phits) {
                assert!(!h.title.is_empty());
                assert!(!h.subtitle.is_empty());
                assert!(DeepLink::parse(&h.deep_link).is_some());
            }
        }

        {
            assert!(library_hits(&Library::default(), "anything").is_empty());
            assert!(library_hits(&Library::default(), "").is_empty());
        }

        {
            use ui::search::SearchRegistry;
            let lib = seed::sample_library();

            let mut registry = SearchRegistry::new();
            registry.register(Box::new(source(&lib)));
            assert_eq!(registry.provider_count(), 1);

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

        {
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
