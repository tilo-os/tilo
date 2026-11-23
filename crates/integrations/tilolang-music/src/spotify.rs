//! Spotify's Web API as a second catalogue Music asks, and the person's own Spotify: their
//! sign-in, the songs and playlists they keep, the devices Spotify plays on, and the controls
//! that steer playback on them.
//!
//! The sign-in is the system's: the OAuth grant is sealed in this device's account vault
//! (`tilo-spotify`, the same store Settings' accounts and mail read), never in the app's
//! records, and the person signs in on Spotify's own page, which the app shows them as a link.
//! The grant is renewed by the store, so the connector holds only the access token it reads
//! with.
//!
//! Catalogue answers become the app's own records through the same conversions every answer
//! goes through ([`crate::origin`]): only [`SpotifyReply`]s leave this module.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_spotify::AccountId;
use tilo_spotify::ownership::{
    Authorization, Capability, CredentialStore, OAuthIssuer, OAuthUse, TokenSet,
};

use crate::service::{
    AlbumItem, AlbumPage, ArtistItem, ArtistPage, HomePage, HomeSection, Item, PlaylistItem,
    SearchPage, ServiceError, SpotifyDevice, SpotifyOp, SpotifyReply, TrackItem,
};

/// The account the grant is kept under in the system's account vault. It is the device's, shared
/// by every app that asks Spotify, and never a field of Music's library.
const ACCOUNT: &str = "music.spotify";

/// What the grant is for: Music of Spotify's OAuth client.
const USE: OAuthUse = OAuthUse::new(OAuthIssuer::Spotify, Capability::Music);

/// Larger answers are refused rather than kept: a page of songs is far below this.
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// The one Spotify this process asks: built the first time an effect asks for it.
static SHARED: OnceLock<Option<Arc<Spotify>>> = OnceLock::new();

/// The process's Spotify, or why the process has none.
pub fn shared() -> Result<Arc<Spotify>, ServiceError> {
    SHARED
        .get_or_init(|| Spotify::new().ok().map(Arc::new))
        .clone()
        .ok_or_else(|| ServiceError("Spotify's client could not be started".into()))
}

/// Spotify over the system account vault and the device's network.
pub struct Spotify {
    store: CredentialStore,
    http: reqwest::blocking::Client,
    /// The sign-in the person is giving on Spotify's page, between [`SpotifyOp::Connect`] and
    /// [`SpotifyOp::Finish`].
    pending: Mutex<Option<Authorization>>,
}

impl Spotify {
    /// # Errors
    /// The network client could not be built.
    pub fn new() -> Result<Self, ServiceError> {
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .build()
            .map_err(|error| ServiceError(format!("Spotify's network client: {error}")))?;
        Ok(Self {
            store: CredentialStore::live(),
            http,
            pending: Mutex::new(None),
        })
    }

    fn account() -> AccountId {
        AccountId::from(ACCOUNT.to_string())
    }

    fn token(&self, force: bool) -> Result<String, ServiceError> {
        self.store
            .oauth_access_token(&Self::account(), USE, force)
            .map(|token| token.to_string())
            .map_err(|error| {
                ServiceError(format!(
                    "Spotify needs a sign-in, or its sign-in was not renewed: {error}"
                ))
            })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, ServiceError> {
        self.send(
            self.http.get(format!("https://api.spotify.com/v1/{path}")),
            query,
            None,
        )
    }

    fn put(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, ServiceError> {
        self.send(
            self.http.put(format!("https://api.spotify.com/v1/{path}")),
            query,
            body,
        )
    }

    fn post(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, ServiceError> {
        self.send(
            self.http.post(format!("https://api.spotify.com/v1/{path}")),
            query,
            body,
        )
    }

    fn send(
        &self,
        request: reqwest::blocking::RequestBuilder,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, ServiceError> {
        let mut request = request.bearer_auth(&*self.token(false)?).query(query);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|error| ServiceError(format!("Spotify was not reached: {error}")))?;
        let status = response.status();
        let text = response
            .text()
            .map_err(|error| ServiceError(format!("Spotify's answer was cut short: {error}")))?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_RESPONSE {
            return Err(ServiceError("Spotify's answer was too large".into()));
        }
        if !status.is_success() {
            let said = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|value| {
                    value
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| status.canonical_reason().unwrap_or("no reason").to_owned());
            return Err(ServiceError(format!("Spotify said {said}")));
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text)
            .map_err(|error| ServiceError(format!("Spotify's answer could not be read: {error}")))
    }

    /// What Spotify answers to `op`.
    ///
    /// # Errors
    ///
    /// The network, the person's sign-in, or Spotify's own refusal: why.
    pub fn op(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        match op {
            SpotifyOp::Connect => self.connect(),
            SpotifyOp::Finish => self.finish(),
            SpotifyOp::Disconnect => self.disconnect(),
            SpotifyOp::Status => self.status(),
            SpotifyOp::Devices => self.devices(),
            SpotifyOp::Transfer { device } => self.transfer(device),
            SpotifyOp::Play { uri, device } => self.play(uri, device),
            SpotifyOp::Pause { device } => self.pause(device),
            SpotifyOp::Next { device } => self.step("next", device),
            SpotifyOp::Previous { device } => self.step("previous", device),
            SpotifyOp::Now => self.now(),
            SpotifyOp::Search { query } => self.search(query),
            SpotifyOp::Home => self.home(),
            SpotifyOp::Album { key } => self.album(key),
            SpotifyOp::Artist { key } => self.artist(key),
            SpotifyOp::Playlist { key } => self.playlist(key),
        }
    }

    fn connect(&self) -> Result<SpotifyReply, ServiceError> {
        let authorization = self
            .store
            .begin_sign_in_for(USE, None)
            .map_err(|error| ServiceError(format!("Spotify's sign-in could not begin: {error}")))?;
        let url = authorization.url().to_owned();
        *self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(authorization);
        Ok(SpotifyReply {
            url,
            ..SpotifyReply::default()
        })
    }

    fn finish(&self) -> Result<SpotifyReply, ServiceError> {
        let authorization = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| ServiceError("Spotify has no sign-in waiting".into()))?;
        let tokens: TokenSet = CredentialStore::finish_sign_in(authorization)
            .map_err(|error| ServiceError(format!("Spotify's sign-in did not finish: {error}")))?;
        self.store
            .seal_oauth(&Self::account(), &tokens)
            .map_err(|error| ServiceError(format!("Spotify's sign-in was not kept: {error}")))?;
        self.status()
    }

    fn disconnect(&self) -> Result<SpotifyReply, ServiceError> {
        self.store
            .forget_oauth(&Self::account())
            .map_err(|error| ServiceError(format!("Spotify's sign-in was not forgotten: {error}")))
    }

    fn status(&self) -> Result<SpotifyReply, ServiceError> {
        let me = self.get("me", &[])?;
        Ok(SpotifyReply {
            account: text_of(&me, "display_name")
                .or_else(|| text_of(&me, "id"))
                .unwrap_or_default(),
            product: text_of(&me, "product").unwrap_or_default(),
            ..SpotifyReply::default()
        })
    }

    fn devices(&self) -> Result<SpotifyReply, ServiceError> {
        let places = self.get("me/player/devices", &[])?;
        let playing = self.get("me/player", &[]).unwrap_or(Value::Null);
        let devices = places
            .get("devices")
            .and_then(Value::as_array)
            .map(|devices| devices.iter().filter_map(device_of).collect())
            .unwrap_or_default();
        Ok(SpotifyReply {
            devices,
            active: playing
                .get("device")
                .and_then(|device| text_of(device, "id"))
                .unwrap_or_default(),
            playing: playing
                .get("is_playing")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            now: now_of(&playing),
            ..SpotifyReply::default()
        })
    }

    fn transfer(&self, device: &str) -> Result<SpotifyReply, ServiceError> {
        self.put(
            "me/player",
            &[],
            Some(&json!({"device_ids":[device],"play":true})),
        )?;
        self.devices()
    }

    fn play(&self, uri: &str, device: &str) -> Result<SpotifyReply, ServiceError> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.put("me/player/play", query, Some(&json!({"uris":[uri]})))?;
        self.devices()
    }

    fn pause(&self, device: &str) -> Result<SpotifyReply, ServiceError> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.put("me/player/pause", query, None)?;
        self.devices()
    }

    fn step(&self, what: &str, device: &str) -> Result<SpotifyReply, ServiceError> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.post(&format!("me/player/{what}"), query, None)?;
        self.devices()
    }

    fn now(&self) -> Result<SpotifyReply, ServiceError> {
        let playing = self.get("me/player", &[])?;
        Ok(SpotifyReply {
            active: playing
                .get("device")
                .and_then(|device| text_of(device, "id"))
                .unwrap_or_default(),
            playing: playing
                .get("is_playing")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            now: now_of(&playing),
            ..SpotifyReply::default()
        })
    }

    fn search(&self, query: &str) -> Result<SpotifyReply, ServiceError> {
        let found = self.get(
            "search",
            &[
                ("q", query),
                ("type", "track,album,artist,playlist"),
                ("limit", "20"),
            ],
        )?;
        let items = |what: &str| {
            found
                .get(what)
                .and_then(|kind| kind.get("items"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Ok(SpotifyReply {
            search: Some(SearchPage {
                query: query.to_owned(),
                top: None,
                tracks: items("tracks").iter().filter_map(track_of).collect(),
                albums: items("albums").iter().filter_map(album_of).collect(),
                artists: items("artists").iter().filter_map(artist_of).collect(),
                playlists: items("playlists").iter().filter_map(playlist_of).collect(),
            }),
            ..SpotifyReply::default()
        })
    }

    fn home(&self) -> Result<SpotifyReply, ServiceError> {
        let mut sections = Vec::new();
        if let Ok(saved) = self.get("me/tracks", &[("limit", "20")])
            && let Some(items) = saved.get("items").and_then(Value::as_array)
            && !items.is_empty()
        {
            sections.push(HomeSection {
                title: "Liked songs".into(),
                items: items
                    .iter()
                    .filter_map(|saved| saved.get("track"))
                    .filter_map(track_of)
                    .map(Item::Track)
                    .collect(),
            });
        }
        if let Ok(playlists) = self.get("me/playlists", &[("limit", "20")])
            && let Some(items) = playlists.get("items").and_then(Value::as_array)
            && !items.is_empty()
        {
            sections.push(HomeSection {
                title: "Your playlists".into(),
                items: items
                    .iter()
                    .filter_map(playlist_of)
                    .map(Item::Playlist)
                    .collect(),
            });
        }
        Ok(SpotifyReply {
            home: Some(HomePage { sections }),
            ..SpotifyReply::default()
        })
    }

    fn album(&self, key: &str) -> Result<SpotifyReply, ServiceError> {
        let album = self.get(&format!("albums/{key}"), &[])?;
        let tracks = self.get(&format!("albums/{key}/tracks"), &[("limit", "50")])?;
        let songs = tracks
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|track| {
                        let mut song = track_of(track)?;
                        song.album = text_of(&album, "name").unwrap_or_default();
                        song.album_key = key.to_owned();
                        Some(song)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(SpotifyReply {
            album: album_of(&album).map(|album| AlbumPage {
                album,
                tracks: songs,
            }),
            ..SpotifyReply::default()
        })
    }

    fn artist(&self, key: &str) -> Result<SpotifyReply, ServiceError> {
        let artist = self.get(&format!("artists/{key}"), &[])?;
        let top = self.get(
            &format!("artists/{key}/top-tracks"),
            &[("market", "from_token")],
        );
        let songs = top
            .ok()
            .and_then(|top| top.get("tracks").and_then(Value::as_array).cloned())
            .map(|tracks| tracks.iter().filter_map(track_of).collect())
            .unwrap_or_default();
        let releases = self.get(
            &format!("artists/{key}/albums"),
            &[("include_groups", "album,single"), ("limit", "20")],
        );
        let albums = releases
            .ok()
            .and_then(|releases| releases.get("items").and_then(Value::as_array).cloned())
            .map(|albums| albums.iter().filter_map(album_of).collect())
            .unwrap_or_default();
        Ok(SpotifyReply {
            artist: artist_of(&artist).map(|artist| ArtistPage {
                artist,
                tracks: songs,
                albums,
                ..ArtistPage::default()
            }),
            ..SpotifyReply::default()
        })
    }

    fn playlist(&self, key: &str) -> Result<SpotifyReply, ServiceError> {
        let playlist = self.get(&format!("playlists/{key}"), &[])?;
        let items = self.get(&format!("playlists/{key}/items"), &[("limit", "100")])?;
        let songs = items
            .get("items")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|entry| entry.get("item").or_else(|| entry.get("track")))
                    .filter_map(track_of)
                    .collect()
            })
            .unwrap_or_default();
        Ok(SpotifyReply {
            playlist: playlist_of(&playlist).map(|playlist| AlbumPage {
                album: AlbumItem {
                    key: playlist.key.clone(),
                    title: playlist.title.clone(),
                    artist: playlist.subtitle.clone(),
                    cover: playlist.cover.clone(),
                    ..AlbumItem::default()
                },
                tracks: songs,
            }),
            ..SpotifyReply::default()
        })
    }
}

fn text_of(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn first_image(value: &Value) -> String {
    value
        .get("images")
        .and_then(Value::as_array)
        .and_then(|images| images.first())
        .and_then(|image| text_of(image, "url"))
        .unwrap_or_default()
}

fn names(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|people| {
            people
                .iter()
                .filter_map(|person| text_of(person, "name"))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn first_key(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_array)
        .and_then(|people| people.first())
        .and_then(|person| text_of(person, "id"))
        .unwrap_or_default()
}

fn track_of(track: &Value) -> Option<TrackItem> {
    let id = text_of(track, "id")?;
    let album = track.get("album");
    Some(TrackItem {
        key: format!("spotify:track:{id}"),
        title: text_of(track, "name").unwrap_or_default(),
        artist: names(track, "artists"),
        artist_key: first_key(track, "artists"),
        album: album
            .and_then(|album| text_of(album, "name"))
            .unwrap_or_default(),
        album_key: album
            .and_then(|album| text_of(album, "id"))
            .unwrap_or_default(),
        seconds: track
            .get("duration_ms")
            .and_then(Value::as_i64)
            .unwrap_or(0)
            / 1000,
        plays: 0,
        cover: album.map(first_image).unwrap_or_default(),
    })
}

fn album_of(album: &Value) -> Option<AlbumItem> {
    let id = text_of(album, "id")?;
    Some(AlbumItem {
        key: format!("spotify:album:{id}"),
        title: text_of(album, "name").unwrap_or_default(),
        artist: names(album, "artists"),
        artist_key: first_key(album, "artists"),
        year: text_of(album, "release_date")
            .and_then(|date| date.get(..4).and_then(|year| year.parse().ok()))
            .unwrap_or(0),
        kind: text_of(album, "album_type").unwrap_or_default(),
        cover: first_image(album),
    })
}

fn artist_of(artist: &Value) -> Option<ArtistItem> {
    let id = text_of(artist, "id")?;
    Some(ArtistItem {
        key: format!("spotify:artist:{id}"),
        name: text_of(artist, "name").unwrap_or_default(),
        subscribers: artist
            .pointer("/followers/total")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        avatar: first_image(artist),
    })
}

fn playlist_of(playlist: &Value) -> Option<PlaylistItem> {
    let id = text_of(playlist, "id")?;
    Some(PlaylistItem {
        key: format!("spotify:playlist:{id}"),
        title: text_of(playlist, "name").unwrap_or_default(),
        subtitle: playlist
            .pointer("/owner/display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        track_count: playlist
            .pointer("/tracks/total")
            .or_else(|| playlist.pointer("/items/total"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        cover: first_image(playlist),
    })
}

fn device_of(device: &Value) -> Option<SpotifyDevice> {
    let key = text_of(device, "id")?;
    Some(SpotifyDevice {
        key,
        name: text_of(device, "name").unwrap_or_default(),
        kind: text_of(device, "type").unwrap_or_default(),
        active: device
            .get("is_active")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        volume: device
            .pointer("/volume_percent")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    })
}

fn now_of(playing: &Value) -> String {
    let Some(item) = playing.get("item") else {
        return String::new();
    };
    let title = text_of(item, "name").unwrap_or_default();
    let artist = names(item, "artists");
    if title.is_empty() {
        artist
    } else if artist.is_empty() {
        title
    } else {
        format!("{title} — {artist}")
    }
}
