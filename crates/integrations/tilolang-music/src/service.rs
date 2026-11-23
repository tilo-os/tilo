//! The service the music origin asks: what a search, a page, a stream, lyrics and a pinned
//! song are, in the client's own terms.
//!
//! The origin ([`crate::origin`]) holds no protocol: it hands a [`Service`] a question and
//! writes the answer into the package's records. Two are provided: [`Live`] asks YouTube Music
//! through the platform's InnerTube client, in the connector process the system runs for the
//! app (which holds the network), and a test or a development session can hand its own instead.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::backend::MusicBackend as _;

/// A song, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackItem {
    /// The catalogue's id: the video it plays from.
    pub key: String,
    pub title: String,
    pub artist: String,
    pub artist_key: String,
    pub album: String,
    pub album_key: String,
    /// How long it runs, in seconds.
    pub seconds: i64,
    pub plays: i64,
    /// The cover's content address, as this device holds it, or empty.
    pub cover: String,
}

/// An album, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumItem {
    pub key: String,
    pub title: String,
    pub artist: String,
    pub artist_key: String,
    pub year: i64,
    /// `album`, `single`, `ep` or `release`.
    pub kind: String,
    pub cover: String,
}

/// An artist, as the service says they are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistItem {
    pub key: String,
    pub name: String,
    pub subscribers: i64,
    pub avatar: String,
}

/// A catalogue playlist, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: i64,
    pub cover: String,
}

/// One thing a search or the home page names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Item {
    Track(TrackItem),
    Album(AlbumItem),
    Artist(ArtistItem),
    Playlist(PlaylistItem),
}

impl Item {
    /// The id the item is kept by.
    #[must_use]
    pub fn key(&self) -> &str {
        match self {
            Self::Track(t) => &t.key,
            Self::Album(a) => &a.key,
            Self::Artist(a) => &a.key,
            Self::Playlist(p) => &p.key,
        }
    }

    /// What the item is, as a record's `kind` says it: `track`, `album`, `artist`, `playlist`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Track(_) => "track",
            Self::Album(_) => "album",
            Self::Artist(_) => "artist",
            Self::Playlist(_) => "playlist",
        }
    }
}

/// What a search found.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchPage {
    /// The query, as it was asked: what its rows were found for.
    pub query: String,
    pub top: Option<Item>,
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<PlaylistItem>,
}

/// One titled row of the home page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeSection {
    pub title: String,
    pub items: Vec<Item>,
}

/// The home page: charts, new releases and top artists, in titled sections.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomePage {
    pub sections: Vec<HomeSection>,
}

/// An album's page: the album and its songs, in order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlbumPage {
    pub album: AlbumItem,
    pub tracks: Vec<TrackItem>,
}

/// An artist's page: who they are, their top songs, their releases and who is similar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistPage {
    pub artist: ArtistItem,
    pub description: String,
    pub header: String,
    pub wikipedia: String,
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub similar: Vec<ArtistItem>,
}

/// One line of a song's words, and when it is sung.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LyricLine {
    /// How far into the song the line starts, in milliseconds; 0 for words that are not timed.
    pub at_ms: i64,
    pub text: String,
}

/// A song's words.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lyrics {
    /// Whether the lines carry their times.
    pub synced: bool,
    pub lines: Vec<LyricLine>,
    /// Who wrote them, or where they were found, when the service says.
    pub credit: String,
}

/// Why a question could not be answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceError(pub String);

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ServiceError {}

/// What the origin asks of a service. Never waits on the network itself: the service does.
pub trait Service: Send + Sync {
    /// What the catalogue finds for `query`.
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError>;

    /// The home page the catalogue puts first.
    fn home(&self) -> Result<HomePage, ServiceError>;

    /// An artist's page.
    fn artist(&self, key: &str) -> Result<ArtistPage, ServiceError>;

    /// An album's page.
    fn album(&self, key: &str) -> Result<AlbumPage, ServiceError>;

    /// An album a catalogue playlist names, by the playlist's key: the playlist's songs, read
    /// as an album's page. A playlist the catalogue publishes is a list of songs the app can
    /// keep like any album.
    fn playlist(&self, key: &str) -> Result<AlbumPage, ServiceError>;

    /// The address a song's bytes play from.
    fn stream(&self, key: &str) -> Result<String, ServiceError>;

    /// A song's words, found by what it is called and who sings it.
    fn lyrics(&self, title: &str, artist: &str) -> Result<Lyrics, ServiceError>;

    /// Keep the bytes at `url` on this device as the song called `name`, answering the content
    /// address every window of the workspace reads them at (`tilo-blob://…`): what keeping a
    /// song offline is.
    fn pin(&self, url: &str, name: &str) -> Result<String, ServiceError>;
}

/// The service over YouTube Music's own client (`tilo-innertube`, which stands on rustypipe).
pub struct Live;

impl Live {
    /// The service over the one client of the process, which is built the first time it is
    /// asked, inside the app's runtime: a window that shows no music resource never builds it,
    /// and one that does starts no connection until its first search.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "live")]
impl Live {
    fn backend() -> Result<crate::backend::live::RustyPipeBackend, ServiceError> {
        crate::backend::live::RustyPipeBackend::new().map_err(|e| ServiceError(e.0))
    }
}

#[cfg(feature = "live")]
impl Service for Live {
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError> {
        let bundle = Self::backend()?
            .search_all(query)
            .map_err(|e| ServiceError(e.0))?;
        let track = |t: crate::backend::TrackHit| TrackItem {
            key: t.video_id,
            title: t.title,
            artist: t.artist,
            artist_key: t.artist_id.unwrap_or_default(),
            album: t.album.unwrap_or_default(),
            album_key: t.album_id.unwrap_or_default(),
            seconds: i64::from(t.duration_s),
            plays: 0,
            cover: t.cover_url,
        };
        let album = |a: crate::backend::AlbumHit| AlbumItem {
            key: a.id,
            title: a.title,
            artist: a.artist,
            artist_key: a.artist_id.unwrap_or_default(),
            year: i64::from(a.year),
            kind: a.kind.label().to_lowercase(),
            cover: a.cover_url,
        };
        let artist = |a: crate::backend::ArtistHit| ArtistItem {
            key: a.id,
            name: a.name,
            subscribers: i64::try_from(a.subscriber_count).unwrap_or(i64::MAX),
            avatar: a.avatar_url,
        };
        let playlist = |p: crate::backend::PlaylistHit| PlaylistItem {
            key: p.id,
            title: p.title,
            subtitle: p.subtitle,
            track_count: i64::try_from(p.track_count).unwrap_or(i64::MAX),
            cover: p.cover_url,
        };
        let top = bundle.top.map(|top| match top {
            crate::backend::SearchTop::Track(t) => Item::Track(track(t)),
            crate::backend::SearchTop::Album(a) => Item::Album(album(a)),
            crate::backend::SearchTop::Artist(a) => Item::Artist(artist(a)),
            crate::backend::SearchTop::Playlist(p) => Item::Playlist(playlist(p)),
        });
        Ok(SearchPage {
            query: query.to_owned(),
            top,
            tracks: bundle.tracks.into_iter().map(track).collect(),
            albums: bundle.albums.into_iter().map(album).collect(),
            artists: bundle.artists.into_iter().map(artist).collect(),
            playlists: bundle.playlists.into_iter().map(playlist).collect(),
        })
    }

    fn home(&self) -> Result<HomePage, ServiceError> {
        let content = Self::backend()?.home().map_err(|e| ServiceError(e.0))?;
        let sections = content
            .sections
            .into_iter()
            .map(|section| HomeSection {
                title: section.title,
                items: section
                    .items
                    .into_iter()
                    .map(|item| {
                        let kind = match item.kind {
                            crate::backend::HomeItemKind::Album => "album",
                            crate::backend::HomeItemKind::Artist => "artist",
                            crate::backend::HomeItemKind::Playlist => "playlist",
                            crate::backend::HomeItemKind::Track => "track",
                        };
                        Item::Track(TrackItem {
                            key: if item.video_id.is_empty() {
                                item.id
                            } else {
                                item.video_id
                            },
                            title: item.title,
                            artist: item.artist_name,
                            artist_key: item.artist_id,
                            album: item.album_title,
                            album_key: item.album_id,
                            seconds: i64::from(item.duration_s),
                            plays: 0,
                            cover: item.cover_url,
                        })
                        .with_kind_hint(kind)
                    })
                    .collect(),
            })
            .collect();
        Ok(HomePage { sections })
    }

    fn artist(&self, key: &str) -> Result<ArtistPage, ServiceError> {
        let page = Self::backend()?
            .artist(key)
            .map_err(|e| ServiceError(e.0))?;
        Ok(ArtistPage {
            artist: ArtistItem {
                key: page.id,
                name: page.name,
                subscribers: i64::try_from(page.subscriber_count).unwrap_or(i64::MAX),
                avatar: page.header_url.clone(),
            },
            description: page.description,
            header: page.header_url,
            wikipedia: page.wikipedia_url,
            tracks: page
                .tracks
                .into_iter()
                .map(|t| TrackItem {
                    key: t.video_id,
                    title: t.title,
                    artist: t.artist,
                    artist_key: t.artist_id.unwrap_or_default(),
                    album: t.album.unwrap_or_default(),
                    album_key: t.album_id.unwrap_or_default(),
                    seconds: i64::from(t.duration_s),
                    plays: 0,
                    cover: t.cover_url,
                })
                .collect(),
            albums: page
                .albums
                .into_iter()
                .map(|a| AlbumItem {
                    key: a.id,
                    title: a.title,
                    artist: String::new(),
                    artist_key: String::new(),
                    year: i64::from(a.year),
                    kind: a.kind.label().to_lowercase(),
                    cover: a.cover_url,
                })
                .collect(),
            similar: page
                .similar
                .into_iter()
                .map(|a| ArtistItem {
                    key: a.id,
                    name: a.name,
                    subscribers: i64::try_from(a.subscriber_count).unwrap_or(i64::MAX),
                    avatar: a.avatar_url,
                })
                .collect(),
        })
    }

    fn album(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        let page = Self::backend()?.album(key).map_err(|e| ServiceError(e.0))?;
        Ok(AlbumPage {
            album: AlbumItem {
                key: page.id,
                title: page.title,
                artist: page.artist_name,
                artist_key: page.artist_id,
                year: i64::from(page.year),
                kind: "album".to_owned(),
                cover: page.cover_url,
            },
            tracks: page
                .tracks
                .into_iter()
                .map(|t| TrackItem {
                    key: t.video_id,
                    title: t.title,
                    artist: t.artist,
                    artist_key: t.artist_id.unwrap_or_default(),
                    album: t.album.unwrap_or_default(),
                    album_key: t.album_id.unwrap_or_default(),
                    seconds: i64::from(t.duration_s),
                    plays: 0,
                    cover: t.cover_url,
                })
                .collect(),
        })
    }

    fn playlist(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        let page = Self::backend()?
            .playlist(key)
            .map_err(|e| ServiceError(e.0))?;
        Ok(AlbumPage {
            album: AlbumItem {
                key: page.id,
                title: page.title,
                artist: page.subtitle,
                artist_key: String::new(),
                year: 0,
                kind: "playlist".to_owned(),
                cover: page.cover_url,
            },
            tracks: page
                .tracks
                .into_iter()
                .map(|t| TrackItem {
                    key: t.video_id,
                    title: t.title,
                    artist: t.artist,
                    artist_key: t.artist_id.unwrap_or_default(),
                    album: t.album.unwrap_or_default(),
                    album_key: t.album_id.unwrap_or_default(),
                    seconds: i64::from(t.duration_s),
                    plays: 0,
                    cover: t.cover_url,
                })
                .collect(),
        })
    }

    fn stream(&self, key: &str) -> Result<String, ServiceError> {
        let resolved = Self::backend()?
            .resolve_audio(key)
            .map_err(|e| ServiceError(e.0))?;
        Ok(resolved.url)
    }

    fn lyrics(&self, title: &str, artist: &str) -> Result<Lyrics, ServiceError> {
        let found = crate::lyrics::fetch(title, artist).map_err(ServiceError)?;
        Ok(Lyrics {
            synced: found.synced,
            credit: "lrclib".to_owned(),
            lines: found
                .lines
                .into_iter()
                .map(|line| LyricLine {
                    at_ms: line
                        .at_ms
                        .map_or(0, |at| i64::try_from(at).unwrap_or(i64::MAX)),
                    text: line.text,
                })
                .collect(),
        })
    }

    fn pin(&self, url: &str, name: &str) -> Result<String, ServiceError> {
        crate::files::pin(url, name).map_err(ServiceError)
    }
}

impl Item {
    /// An item read as the kind the home page said it is: a home tile names a song, an album,
    /// an artist or a playlist, and the record it is written as carries that kind.
    fn with_kind_hint(self, kind: &str) -> Self {
        match (self, kind) {
            (Item::Track(t), "album") => Item::Album(AlbumItem {
                key: t.key,
                title: t.title,
                artist: t.artist,
                artist_key: t.artist_key,
                year: 0,
                kind: "album".to_owned(),
                cover: t.cover,
            }),
            (Item::Track(t), "artist") => Item::Artist(ArtistItem {
                key: t.artist_key,
                name: t.artist,
                subscribers: 0,
                avatar: t.cover,
            }),
            (Item::Track(t), "playlist") => Item::Playlist(PlaylistItem {
                key: t.key,
                title: t.title,
                subtitle: t.artist,
                track_count: 0,
                cover: t.cover,
            }),
            (item, _) => item,
        }
    }
}

/// A service that answers from a script, for a test or a development session: the same answers
/// every time, in the order they were written.
#[derive(Default)]
pub struct Scripted {
    pub searches: std::collections::BTreeMap<String, SearchPage>,
    pub home: HomePage,
    pub artists: std::collections::BTreeMap<String, ArtistPage>,
    pub albums: std::collections::BTreeMap<String, AlbumPage>,
    pub streams: std::collections::BTreeMap<String, String>,
    pub lyrics: std::collections::BTreeMap<String, Lyrics>,
    /// The content address each pinned URL is kept at.
    pub pinned: std::collections::BTreeMap<String, String>,
}

impl Service for Scripted {
    fn search(&self, query: &str) -> Result<SearchPage, ServiceError> {
        self.searches
            .get(query)
            .cloned()
            .ok_or_else(|| ServiceError(format!("nothing scripted for {query:?}")))
    }

    fn home(&self) -> Result<HomePage, ServiceError> {
        Ok(self.home.clone())
    }

    fn artist(&self, key: &str) -> Result<ArtistPage, ServiceError> {
        self.artists
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted artist {key:?}")))
    }

    fn album(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        self.albums
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted album {key:?}")))
    }

    fn playlist(&self, key: &str) -> Result<AlbumPage, ServiceError> {
        self.albums
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted playlist {key:?}")))
    }

    fn stream(&self, key: &str) -> Result<String, ServiceError> {
        self.streams
            .get(key)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted stream for {key:?}")))
    }

    fn lyrics(&self, title: &str, _artist: &str) -> Result<Lyrics, ServiceError> {
        self.lyrics
            .get(title)
            .cloned()
            .ok_or_else(|| ServiceError(format!("no scripted lyrics for {title:?}")))
    }

    fn pin(&self, url: &str, _name: &str) -> Result<String, ServiceError> {
        self.pinned
            .get(url)
            .cloned()
            .ok_or_else(|| ServiceError(format!("nothing scripted for {url:?}")))
    }
}

/// A scripted service, ready to share with an origin.
#[must_use]
pub fn scripted(scripted: Scripted) -> Arc<dyn Service> {
    Arc::new(scripted)
}
