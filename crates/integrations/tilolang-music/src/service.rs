//! The service the music origin asks: what a search, a page, a stream, lyrics and a pinned
//! song are, in the client's own terms.
//!
//! The origin ([`crate::origin`]) holds no protocol: it hands a [`Service`] a question and
//! writes the answer into the package's records. Two are provided: [`Live`] asks YouTube Music
//! through the platform's InnerTube client, in the connector process the system runs for the
//! app (which holds the network), and a test or a development session can hand its own instead.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

// The catalogue backend's methods the live service asks.
#[cfg(feature = "live")]
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
    /// What the catalogue calls the artist's kind of music, as it lists them: empty where the
    /// catalogue names none.
    #[serde(default)]
    pub genres: Vec<String>,
}

/// A catalogue playlist, as the service says it is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistItem {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub track_count: i64,
    pub cover: String,
    /// What the playlist says about itself; empty when it says nothing.
    #[serde(default)]
    pub description: String,
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

    /// What Spotify answers to `op`: its catalogue, and the person's own sign-in, library,
    /// devices and playback. A catalogue that is not Spotify says so.
    fn spotify(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        let _ = op;
        Err(ServiceError("this catalogue has no Spotify".into()))
    }
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
            genres: Vec::new(),
        };
        let playlist = |p: crate::backend::PlaylistHit| PlaylistItem {
            key: p.id,
            title: p.title,
            subtitle: p.subtitle,
            track_count: i64::try_from(p.track_count).unwrap_or(i64::MAX),
            cover: p.cover_url,
            description: String::new(),
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
                genres: Vec::new(),
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
                    genres: Vec::new(),
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

    fn spotify(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        crate::spotify::shared()?.op(op)
    }
}

/// What the live service reads of an item the catalogue's home page lists.
#[cfg(feature = "live")]
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
                genres: Vec::new(),
            }),
            (Item::Track(t), "playlist") => Item::Playlist(PlaylistItem {
                key: t.key,
                title: t.title,
                subtitle: t.artist,
                track_count: 0,
                cover: t.cover,
                description: String::new(),
            }),
            (item, _) => item,
        }
    }
}

/// A place Spotify plays on: a phone, a computer, a speaker or the person's browser.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotifyDevice {
    /// The device's id, as Spotify names it.
    pub key: String,
    pub name: String,
    /// `computer`, `smartphone`, `speaker`, `tv`, `cast` or another it names.
    pub kind: String,
    /// Whether what plays plays here.
    pub active: bool,
    /// How loud it is, `0..=100`, when Spotify says.
    pub volume: i64,
}

/// What a question to Spotify's own Web API is: the parts of a catalogue the app shows, and
/// the person's own sign-in, library and devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpotifyOp {
    /// Begin the sign-in: the answer carries the page the person signs in on.
    Connect,
    /// Take the sign-in the person gave on Spotify's page.
    Finish,
    /// Forget this device's Spotify sign-in.
    Disconnect,
    /// Who is signed in, and whether Spotify lets them play.
    Status,
    /// The places Spotify plays on.
    Devices,
    /// Move what plays to `device`, from the beginning.
    Transfer {
        device: String,
    },
    /// Play `uri` on `device` (its own, when empty).
    Play {
        uri: String,
        device: String,
    },
    /// Pause what plays on `device`.
    Pause {
        device: String,
    },
    /// What plays after this one plays.
    Next {
        device: String,
    },
    /// What plays before this one plays.
    Previous {
        device: String,
    },
    /// What plays now, and where.
    Now,
    /// What Spotify's catalogue finds for `query`.
    Search {
        query: String,
    },
    /// What the person keeps in Spotify: their songs and playlists.
    Home,
    Album {
        key: String,
    },
    Artist {
        key: String,
    },
    Playlist {
        key: String,
    },
    /// Keep a song in Spotify's library, or take it out.
    Like {
        key: String,
        liked: bool,
    },
    /// Keep an album in Spotify's library, or take it out.
    SaveAlbum {
        key: String,
        keep: bool,
    },
    /// Follow an artist, or stop following them.
    Follow {
        key: String,
        follow: bool,
    },
    /// Make a private playlist of the person's own, called `name`.
    CreatePlaylist {
        name: String,
    },
    /// Give a playlist of the person's a new name and description.
    EditPlaylist {
        key: String,
        name: String,
        description: String,
    },
    /// Put `uri` at the end of a playlist.
    AddToPlaylist {
        key: String,
        uri: String,
    },
    /// Take `uri` out of a playlist, every time it is in it.
    RemoveFromPlaylist {
        key: String,
        uri: String,
    },
    /// What plays next on Spotify: its own queue, which the app shows but never edits.
    Queue,
    /// Put `uri` after what plays on Spotify.
    AddToQueue {
        uri: String,
    },
    /// One more page of what the person keeps: `tracks`, `albums`, `playlists` or `artists`,
    /// from `offset`.
    SavedPage {
        kind: String,
        offset: i64,
    },
}

impl SpotifyOp {
    /// The name a reply is looked up by in a script: the op without its arguments.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Finish => "finish",
            Self::Disconnect => "disconnect",
            Self::Status => "status",
            Self::Devices => "devices",
            Self::Transfer { .. } => "transfer",
            Self::Play { .. } => "play",
            Self::Pause { .. } => "pause",
            Self::Next { .. } => "next",
            Self::Previous { .. } => "previous",
            Self::Now => "now",
            Self::Search { .. } => "search",
            Self::Home => "home",
            Self::Album { .. } => "album",
            Self::Artist { .. } => "artist",
            Self::Playlist { .. } => "playlist",
            Self::Like { .. } => "like",
            Self::SaveAlbum { .. } => "save_album",
            Self::Follow { .. } => "follow",
            Self::CreatePlaylist { .. } => "create_playlist",
            Self::EditPlaylist { .. } => "edit_playlist",
            Self::AddToPlaylist { .. } => "playlist_add",
            Self::RemoveFromPlaylist { .. } => "playlist_remove",
            Self::Queue => "queue",
            Self::AddToQueue { .. } => "add_to_queue",
            Self::SavedPage { .. } => "saved_page",
        }
    }
}

/// One page of what the person keeps in Spotify: a section of the home page, and where the page
/// after it starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotifyPage {
    /// `tracks`, `albums`, `playlists` or `artists`: which of the person's collections.
    pub kind: String,
    pub section: HomeSection,
    /// The offset the next page starts at; 0 when Spotify said there is no more.
    pub next: i64,
}

/// Whether something holds with Spotify: one plain boolean on the wire, and a named type here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Allowed(pub bool);

impl Allowed {
    #[must_use]
    pub const fn yes(self) -> bool {
        self.0
    }
}

impl From<bool> for Allowed {
    fn from(allowed: bool) -> Self {
        Self(allowed)
    }
}

/// What Spotify answered: a catalogue page, the person's sign-in, or their devices.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotifyReply {
    /// The page the person signs in on, from [`SpotifyOp::Connect`].
    pub url: String,
    /// What the person is called, signed in.
    pub account: String,
    /// `premium`, `free`, `open` or empty, as Spotify says.
    pub product: String,
    /// The devices Spotify plays on.
    pub devices: Vec<SpotifyDevice>,
    /// The device what plays plays on, empty when nothing does.
    pub active: String,
    /// What plays now, as people read it: the song and who sings it.
    pub now: String,
    /// Whether it plays.
    pub playing: Allowed,
    pub search: Option<SearchPage>,
    pub home: Option<HomePage>,
    pub album: Option<AlbumPage>,
    pub artist: Option<ArtistPage>,
    pub playlist: Option<AlbumPage>,
    /// What plays next on Spotify, in its order, from [`SpotifyOp::Queue`].
    #[serde(default)]
    pub queue: Vec<TrackItem>,
    /// Pages of what the person keeps, from the home page and [`SpotifyOp::SavedPage`].
    #[serde(default)]
    pub pages: Vec<SpotifyPage>,
    /// Whether the song a like or unlike named is in the library now, read back.
    #[serde(default)]
    pub liked: Allowed,
    /// Whether the album a save or removal named is in the library now, read back.
    #[serde(default)]
    pub saved: Allowed,
    /// Whether the person follows the artist a follow or unfollow named, read back.
    #[serde(default)]
    pub followed: Allowed,
    /// The playlist a create or edit made, as Spotify has it now.
    #[serde(default)]
    pub made: Option<PlaylistItem>,
    /// Spotify says this sign-in is over: the person connects again. Nothing else in it is an
    /// answer to what was asked.
    #[serde(default)]
    pub expired: Allowed,
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
    /// What Spotify answers, by [`SpotifyOp::name`].
    pub spotify: std::collections::BTreeMap<String, SpotifyReply>,
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

    fn spotify(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        self.spotify
            .get(op.name())
            .cloned()
            .ok_or_else(|| ServiceError(format!("no Spotify scripted for {op:?}")))
    }
}

/// A scripted service, ready to share with an origin.
#[must_use]
pub fn scripted(scripted: Scripted) -> Arc<dyn Service> {
    Arc::new(scripted)
}
