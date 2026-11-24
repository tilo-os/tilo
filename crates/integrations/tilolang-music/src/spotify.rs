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
    Authorization, Capability, CredentialError, CredentialStore, OAuthIssuer, OAuthUse, TokenSet,
};

use crate::service::{
    AlbumItem, AlbumPage, ArtistItem, ArtistPage, HomePage, HomeSection, Item, PlaylistItem,
    SearchPage, ServiceError, SpotifyDevice, SpotifyOp, SpotifyPage, SpotifyReply, TrackItem,
};

/// The account the grant is kept under in the system's account vault. It is the device's, shared
/// by every app that asks Spotify, and never a field of Music's library.
const ACCOUNT: &str = "music.spotify";

/// What the grant is for: Music of Spotify's OAuth client.
const USE: OAuthUse = OAuthUse::new(OAuthIssuer::Spotify, Capability::Music);

/// Larger answers are refused rather than kept: a page of songs is far below this.
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// What the person keeps in Spotify, each a page of its own.
const SAVED_KINDS: [&str; 4] = ["tracks", "albums", "playlists", "artists"];

/// How many of a collection one page asks for, as Spotify's largest page.
const SAVED_PAGE: &str = "50";

/// The same page's size as a number, for where the next page starts.
const SAVED_PAGE_ITEMS: i64 = 50;

/// Where Spotify's Web API is: every question is a path under it.
const API: &str = "https://api.spotify.com/v1";

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
    /// Where the Web API is: Spotify's own, or a stand-in a test asks.
    api: String,
    /// The sign-in the person is giving on Spotify's page, between [`SpotifyOp::Connect`] and
    /// [`SpotifyOp::Finish`].
    pending: Mutex<Option<Authorization>>,
}

/// Why Spotify did not answer what it was asked.
enum Refusal {
    /// Spotify will not take this sign-in any more, or could not renew it: the person connects
    /// again, and nothing else is wrong.
    Expired,
    /// The network, or Spotify's own refusal: why.
    Reason(String),
}

impl From<Refusal> for ServiceError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Expired => {
                ServiceError("Spotify needs a sign-in, or its sign-in was not renewed".into())
            }
            Refusal::Reason(reason) => ServiceError(reason),
        }
    }
}

impl Spotify {
    /// # Errors
    /// The network client could not be built.
    pub fn new() -> Result<Self, ServiceError> {
        Self::over(CredentialStore::live(), API)
    }

    /// Spotify whose Web API stands at `api`, reading the sign-in from `store`: Spotify's own
    /// for a running instance, and a stand-in server for a test.
    ///
    /// # Errors
    /// The network client could not be built.
    pub fn over(store: CredentialStore, api: &str) -> Result<Self, ServiceError> {
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(20))
            .connect_timeout(Duration::from_secs(8))
            .build()
            .map_err(|error| ServiceError(format!("Spotify's network client: {error}")))?;
        Ok(Self {
            store,
            http,
            api: api.trim_end_matches('/').to_owned(),
            pending: Mutex::new(None),
        })
    }

    fn account() -> AccountId {
        AccountId::from(ACCOUNT.to_string())
    }

    fn token(&self, force: bool) -> Result<String, Refusal> {
        self.store
            .oauth_access_token(&Self::account(), USE, force)
            .map(|token| token.to_string())
            .map_err(|error| match error {
                CredentialError::NeedsAuthentication => Refusal::Expired,
                other => Refusal::Reason(format!("Spotify's sign-in could not be read: {other}")),
            })
    }

    fn get(&self, path: &str, query: &[(&str, &str)]) -> Result<Value, Refusal> {
        self.send(reqwest::Method::GET, path, query, None)
    }

    fn put(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, Refusal> {
        self.send(reqwest::Method::PUT, path, query, body)
    }

    fn post(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, Refusal> {
        self.send(reqwest::Method::POST, path, query, body)
    }

    fn delete(
        &self,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, Refusal> {
        self.send(reqwest::Method::DELETE, path, query, body)
    }

    /// One request to Spotify, and its answer read as JSON.
    ///
    /// A token Spotify no longer takes is renewed once and the request asked again: a grant the
    /// person still holds is not over. Only a second refusal is the sign-in being over, which
    /// the app answers by asking the person to connect again.
    fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, Refusal> {
        let mut renewed = false;
        loop {
            let url = format!("{}/{path}", self.api);
            let mut request = self
                .http
                .request(method.clone(), url)
                .bearer_auth(&*self.token(renewed)?)
                .query(query);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = request
                .send()
                .map_err(|error| Refusal::Reason(format!("Spotify was not reached: {error}")))?;
            let status = response.status();
            let text = response.text().map_err(|error| {
                Refusal::Reason(format!("Spotify's answer was cut short: {error}"))
            })?;
            if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_RESPONSE {
                return Err(Refusal::Reason("Spotify's answer was too large".into()));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                if renewed {
                    return Err(Refusal::Expired);
                }
                renewed = true;
                continue;
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
                return Err(Refusal::Reason(format!("Spotify said {said}")));
            }
            if text.trim().is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_str(&text).map_err(|error| {
                Refusal::Reason(format!("Spotify's answer could not be read: {error}"))
            });
        }
    }

    /// What Spotify answers to `op`.
    ///
    /// # Errors
    ///
    /// The network, or Spotify's own refusal: why. A sign-in Spotify will not renew any more is
    /// not an error but an answer of its own ([`SpotifyReply::expired`]), since it says what the
    /// person has to do rather than that anything failed.
    pub fn op(&self, op: &SpotifyOp) -> Result<SpotifyReply, ServiceError> {
        match self.ask(op) {
            Ok(reply) => Ok(reply),
            Err(Refusal::Expired) => Ok(SpotifyReply {
                expired: true,
                ..SpotifyReply::default()
            }),
            Err(refusal) => Err(refusal.into()),
        }
    }

    fn ask(&self, op: &SpotifyOp) -> Result<SpotifyReply, Refusal> {
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
            SpotifyOp::Like { key, liked } => self.like(key, *liked),
            SpotifyOp::SaveAlbum { key, keep } => self.save_album(key, *keep),
            SpotifyOp::Follow { key, follow } => self.follow(key, *follow),
            SpotifyOp::CreatePlaylist { name } => self.create_playlist(name),
            SpotifyOp::EditPlaylist {
                key,
                name,
                description,
            } => self.edit_playlist(key, name, description),
            SpotifyOp::AddToPlaylist { key, uri } => self.add_to_playlist(key, uri),
            SpotifyOp::RemoveFromPlaylist { key, uri } => self.remove_from_playlist(key, uri),
            SpotifyOp::Queue => self.queue(),
            SpotifyOp::AddToQueue { uri } => self.add_to_queue(uri),
            SpotifyOp::SavedPage { kind, offset } => self.saved_page(kind, *offset),
        }
    }

    fn connect(&self) -> Result<SpotifyReply, Refusal> {
        let authorization = self.store.begin_sign_in_for(USE, None).map_err(|error| {
            Refusal::Reason(format!("Spotify's sign-in could not begin: {error}"))
        })?;
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

    fn finish(&self) -> Result<SpotifyReply, Refusal> {
        let authorization = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or_else(|| Refusal::Reason("Spotify has no sign-in waiting".into()))?;
        let tokens: TokenSet = CredentialStore::finish_sign_in(authorization).map_err(|error| {
            Refusal::Reason(format!("Spotify's sign-in did not finish: {error}"))
        })?;
        self.store
            .seal_oauth(&Self::account(), &tokens)
            .map_err(|error| Refusal::Reason(format!("Spotify's sign-in was not kept: {error}")))?;
        self.status()
    }

    fn disconnect(&self) -> Result<SpotifyReply, Refusal> {
        self.store.forget_oauth(&Self::account()).map_err(|error| {
            Refusal::Reason(format!("Spotify's sign-in was not forgotten: {error}"))
        })?;
        Ok(SpotifyReply::default())
    }

    fn status(&self) -> Result<SpotifyReply, Refusal> {
        let me = self.get("me", &[])?;
        Ok(SpotifyReply {
            account: text_of(&me, "display_name")
                .or_else(|| text_of(&me, "id"))
                .unwrap_or_default(),
            product: text_of(&me, "product").unwrap_or_default(),
            ..SpotifyReply::default()
        })
    }

    fn devices(&self) -> Result<SpotifyReply, Refusal> {
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

    fn transfer(&self, device: &str) -> Result<SpotifyReply, Refusal> {
        self.put(
            "me/player",
            &[],
            Some(&json!({"device_ids":[device],"play":true})),
        )?;
        self.devices()
    }

    fn play(&self, uri: &str, device: &str) -> Result<SpotifyReply, Refusal> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.put("me/player/play", query, Some(&json!({"uris":[uri]})))?;
        self.devices()
    }

    fn pause(&self, device: &str) -> Result<SpotifyReply, Refusal> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.put("me/player/pause", query, None)?;
        self.devices()
    }

    fn step(&self, what: &str, device: &str) -> Result<SpotifyReply, Refusal> {
        let query: &[(&str, &str)] = if device.is_empty() {
            &[]
        } else {
            &[("device_id", device)]
        };
        self.post(&format!("me/player/{what}"), query, None)?;
        self.devices()
    }

    fn now(&self) -> Result<SpotifyReply, Refusal> {
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

    fn search(&self, query: &str) -> Result<SpotifyReply, Refusal> {
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

    fn home(&self) -> Result<SpotifyReply, Refusal> {
        let mut sections = Vec::new();
        let mut pages = Vec::new();
        for kind in SAVED_KINDS {
            // A collection Spotify refuses is left out of the home page rather than failing it;
            // a sign-in that is over stops the whole page, since nothing else is about it.
            match self.saved_page(kind, 0) {
                Ok(reply) => {
                    for page in reply.pages {
                        if !page.section.items.is_empty() {
                            sections.push(page.section.clone());
                            pages.push(page);
                        }
                    }
                }
                Err(Refusal::Expired) => return Err(Refusal::Expired),
                Err(Refusal::Reason(_)) => {}
            }
        }
        Ok(SpotifyReply {
            home: Some(HomePage { sections }),
            pages,
            ..SpotifyReply::default()
        })
    }

    fn album(&self, key: &str) -> Result<SpotifyReply, Refusal> {
        let key = id_of(key);
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
                        key.clone_into(&mut song.album_key);
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

    fn artist(&self, key: &str) -> Result<SpotifyReply, Refusal> {
        let key = id_of(key);
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

    fn playlist(&self, key: &str) -> Result<SpotifyReply, Refusal> {
        let key = id_of(key);
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
                    cover: playlist.cover,
                    ..AlbumItem::default()
                },
                tracks: songs,
            }),
            ..SpotifyReply::default()
        })
    }

    /// Keep a song in the library, or take it out, and say what Spotify holds afterwards.
    fn like(&self, key: &str, liked: bool) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        let query = [("ids", id)];
        if liked {
            self.put("me/tracks", &query, None)?;
        } else {
            self.delete("me/tracks", &query, None)?;
        }
        Ok(SpotifyReply {
            liked: self.kept("me/tracks/contains", &query),
            ..SpotifyReply::default()
        })
    }

    /// Keep an album in the library, or take it out.
    fn save_album(&self, key: &str, keep: bool) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        let query = [("ids", id)];
        if keep {
            self.put("me/albums", &query, None)?;
        } else {
            self.delete("me/albums", &query, None)?;
        }
        Ok(SpotifyReply {
            saved: self.kept("me/albums/contains", &query),
            ..SpotifyReply::default()
        })
    }

    /// Follow an artist, or stop.
    fn follow(&self, key: &str, follow: bool) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        let query = [("type", "artist"), ("ids", id)];
        if follow {
            self.put("me/following", &query, None)?;
        } else {
            self.delete("me/following", &query, None)?;
        }
        Ok(SpotifyReply {
            followed: self.kept("me/following/contains", &query),
            ..SpotifyReply::default()
        })
    }

    /// Whether Spotify says the one thing `query` names is in the library now.
    fn kept(&self, path: &str, query: &[(&str, &str)]) -> bool {
        self.get(path, query)
            .ok()
            .and_then(|answer| answer.as_array().and_then(|items| items.first()).cloned())
            .and_then(|first| first.as_bool())
            .unwrap_or(false)
    }

    /// Make a private playlist of the person's own, and answer it as Spotify has it now.
    fn create_playlist(&self, name: &str) -> Result<SpotifyReply, Refusal> {
        let made = self.post(
            "me/playlists",
            &[],
            Some(&json!({"name": name, "description": "", "public": false})),
        )?;
        Ok(SpotifyReply {
            made: playlist_of(&made),
            ..SpotifyReply::default()
        })
    }

    /// Give a playlist of the person's a new name and description, read back.
    fn edit_playlist(
        &self,
        key: &str,
        name: &str,
        description: &str,
    ) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        self.put(
            &format!("playlists/{id}"),
            &[],
            Some(&json!({"name": name, "description": description})),
        )?;
        self.playlist_reply(id)
    }

    /// Put a song at the end of a playlist, read back.
    fn add_to_playlist(&self, key: &str, uri: &str) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        if uri.is_empty() {
            return Err(Refusal::Reason("the song has no Spotify address".into()));
        }
        self.post(
            &format!("playlists/{id}/items"),
            &[],
            Some(&json!({"uris":[uri]})),
        )?;
        self.playlist_reply(id)
    }

    /// Take a song out of a playlist, every time it is in it, read back.
    fn remove_from_playlist(&self, key: &str, uri: &str) -> Result<SpotifyReply, Refusal> {
        let id = id_of(key);
        if uri.is_empty() {
            return Err(Refusal::Reason("the song has no Spotify address".into()));
        }
        self.delete(
            &format!("playlists/{id}/items"),
            &[],
            Some(&json!({"tracks":[{"uri": uri}]})),
        )?;
        self.playlist_reply(id)
    }

    /// A playlist as a write left it, keyed as the app names it.
    fn playlist_reply(&self, id: &str) -> Result<SpotifyReply, Refusal> {
        let playlist = self.get(&format!("playlists/{id}"), &[])?;
        Ok(SpotifyReply {
            made: playlist_of(&playlist),
            ..SpotifyReply::default()
        })
    }

    /// What plays next on Spotify, in its order.
    fn queue(&self) -> Result<SpotifyReply, Refusal> {
        let queue = self.get("me/player/queue", &[])?;
        let songs = queue
            .get("queue")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(queue_item_of).collect())
            .unwrap_or_default();
        Ok(SpotifyReply {
            now: queue
                .get("currently_playing")
                .map(now_of)
                .unwrap_or_default(),
            queue: songs,
            ..SpotifyReply::default()
        })
    }

    /// Put a song after what plays on Spotify, and answer the queue it went into.
    fn add_to_queue(&self, uri: &str) -> Result<SpotifyReply, Refusal> {
        if uri.is_empty() {
            return Err(Refusal::Reason("the song has no Spotify address".into()));
        }
        self.post("me/player/queue", &[("uri", uri)], None)?;
        self.queue()
    }

    /// One page of what the person keeps, and where the page after it starts.
    fn saved_page(&self, kind: &str, offset: i64) -> Result<SpotifyReply, Refusal> {
        let (title, path) = match kind {
            "tracks" => ("Liked songs", "me/tracks"),
            "albums" => ("Saved albums", "me/albums"),
            "playlists" => ("Your playlists", "me/playlists"),
            "artists" => ("Followed artists", "me/following"),
            _ => {
                return Err(Refusal::Reason(format!(
                    "Spotify keeps no `{kind}` to page"
                )));
            }
        };
        let offset_text = offset.max(0).to_string();
        let mut query = vec![("limit", SAVED_PAGE), ("offset", offset_text.as_str())];
        if kind == "artists" {
            query.push(("type", "artist"));
        }
        let answer = self.get(path, &query)?;
        // Followed artists come one level down, under the kind they are a page of.
        let (items, more) = match kind {
            "artists" => (
                answer
                    .pointer("/artists/items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                answer
                    .pointer("/artists/next")
                    .is_some_and(|next| !next.is_null()),
            ),
            _ => (
                answer
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
                answer.get("next").is_some_and(|next| !next.is_null()),
            ),
        };
        let found: Vec<Item> = items
            .iter()
            .filter_map(|item| match kind {
                "tracks" => item.get("track").and_then(track_of).map(Item::Track),
                "albums" => item.get("album").and_then(album_of).map(Item::Album),
                "playlists" => playlist_of(item).map(Item::Playlist),
                _ => artist_of(item).map(Item::Artist),
            })
            .collect();
        let next = if more {
            offset.max(0) + SAVED_PAGE_ITEMS
        } else {
            0
        };
        Ok(SpotifyReply {
            pages: vec![SpotifyPage {
                kind: kind.to_owned(),
                section: HomeSection {
                    title: title.to_owned(),
                    items: found,
                },
                next,
            }],
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
        genres: artist
            .get("genres")
            .and_then(Value::as_array)
            .map(|genres| {
                genres
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
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
        description: text_of(playlist, "description").unwrap_or_default(),
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
    match playing.get("item") {
        Some(item) => song_line(item),
        None => song_line(playing),
    }
}

/// A song, or an episode of a show, as people read what plays: its name and who it is by.
fn song_line(item: &Value) -> String {
    let title = text_of(item, "name").unwrap_or_default();
    let artist = if names(item, "artists").is_empty() {
        item.get("show")
            .and_then(|show| text_of(show, "name"))
            .unwrap_or_default()
    } else {
        names(item, "artists")
    };
    if title.is_empty() {
        artist
    } else if artist.is_empty() {
        title
    } else {
        format!("{title} — {artist}")
    }
}

/// A song in what plays next on Spotify, as the app shows it: a track, or an episode of a show.
fn queue_item_of(item: &Value) -> Option<TrackItem> {
    if item.get("type").and_then(Value::as_str) != Some("episode") {
        return track_of(item);
    }
    let id = text_of(item, "id")?;
    Some(TrackItem {
        key: format!("spotify:episode:{id}"),
        title: text_of(item, "name").unwrap_or_default(),
        artist: item
            .get("show")
            .and_then(|show| text_of(show, "name"))
            .unwrap_or_default(),
        artist_key: String::new(),
        album: item
            .get("show")
            .and_then(|show| text_of(show, "name"))
            .unwrap_or_default(),
        album_key: String::new(),
        seconds: item.get("duration_ms").and_then(Value::as_i64).unwrap_or(0) / 1000,
        plays: 0,
        cover: item
            .get("images")
            .map(first_image)
            .or_else(|| item.get("show").map(first_image))
            .unwrap_or_default(),
    })
}

/// The id Spotify's API takes for a key the app holds: the part of a `spotify:<kind>:<id>` URI
/// after its kind, or the key as it is when it carries no kind.
fn id_of(key: &str) -> &str {
    key.strip_prefix("spotify:")
        .and_then(|rest| rest.split_once(':').map(|(_, id)| id))
        .unwrap_or(key)
}
