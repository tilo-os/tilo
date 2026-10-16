//! Spotify's public Web API catalogue. No audio URLs, credentials or DRM
//! material enter the collaborative document. Native streaming uses its own
//! authorized transport; a catalogue grant is never treated as an audio URL.
use std::io::Read as _;
use std::sync::Mutex;
use std::time::Duration;
use ui::time::Instant;

use serde_json::Value;
use tilo_spotify::AccountId;
use tilo_spotify::custody::{Capability, Custodian, OAuthIssuer, OAuthUse};

#[path = "spotify_control.rs"]
mod control;

use super::{
    AlbumHit, AlbumKind, AlbumPage, ArtistAlbum, ArtistHit, ArtistPage, BackendError, HomeContent,
    HomeItem, HomeItemKind, HomeSection, MusicBackend, PlaylistHit, PlaylistPage, ResolveOutcome,
    SearchBundle, SearchOutcome, SearchTop, TrackHit,
};

const USE: OAuthUse = OAuthUse::new(OAuthIssuer::Spotify, Capability::Music);
const ACCOUNT: &str = "music.spotify";
const MAX_RESPONSE: u64 = 2 * 1024 * 1024;

pub struct SpotifyBackend {
    custody: Custodian,
    http: reqwest::blocking::Client,
    retry_after: Mutex<Option<Instant>>,
    api_origin: String,
}

impl SpotifyBackend {
    /// # Errors
    /// Network client initialization failed.
    pub fn new() -> Result<Self, BackendError> {
        let http = tilo_net::http::blocking_client()
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| BackendError("Spotify network initialization failed".into()))?;
        Ok(Self {
            custody: Custodian::live(),
            http,
            retry_after: Mutex::new(None),
            api_origin: "https://api.spotify.com/v1/".into(),
        })
    }

    pub(super) fn disconnect(&self) -> Result<(), BackendError> {
        self.custody
            .forget_oauth(&AccountId::from(ACCOUNT.to_string()))
            .map_err(|_| BackendError("Could not forget Spotify sign-in".into()))?;
        self.custody
            .forget_oauth(&AccountId::from("music.spotify.streaming".to_string()))
            .map_err(|_| BackendError("Could not forget Spotify streaming sign-in".into()))
    }

    /// PKCE browser sign-in, with state validation and encrypted system custody.
    pub(super) fn check_connected(&self) -> Result<(), BackendError> {
        self.get("me", &[]).map(|_| ())
    }

    /// Authorize in the system browser and seal the grant in custody.
    ///
    /// # Errors
    /// Missing registration, failed browser launch, rejected sign-in or vault failure.
    pub fn sign_in(&self) -> Result<(), BackendError> {
        self.sign_in_for(USE, ACCOUNT)
    }

    pub(super) fn sign_in_read_only(&self) -> Result<(), BackendError> {
        let authorization = self
            .custody
            .begin_sign_in_for_scopes(USE, &["user-read-private", "user-library-read"], None)
            .map_err(|_| {
                BackendError("Spotify sign-in is unavailable or its callback port is busy".into())
            })?;
        self.finish_sign_in(
            authorization,
            ACCOUNT,
            Some("user-read-private user-library-read"),
        )
    }

    fn sign_in_for(&self, usage: OAuthUse, account: &str) -> Result<(), BackendError> {
        let authorization = self.custody.begin_sign_in_for(usage, None)
            .map_err(|_| BackendError("Spotify sign-in is not configured or its callback port is busy. Register Tilo's public client with http://127.0.0.1:47654/oauth.".into()))?;
        self.finish_sign_in(authorization, account, None)
    }

    fn finish_sign_in(
        &self,
        authorization: tilo_spotify::custody::Authorization,
        account: &str,
        requested_scope: Option<&str>,
    ) -> Result<(), BackendError> {
        ui::portal::open_uri(authorization.url()).map_err(|_| {
            BackendError("Could not open Spotify sign-in in the system browser".into())
        })?;
        let mut tokens = Custodian::finish_sign_in(authorization)
            .map_err(|_| BackendError("Spotify sign-in did not complete. Try again.".into()))?;
        // RFC 6749: omitted scope means the grant matches the requested scope.
        if tokens.scope.is_none() {
            tokens.scope = requested_scope.map(str::to_string);
        }
        self.custody
            .seal_oauth(&AccountId::from(account.to_string()), &tokens)
            .map_err(|_| {
                BackendError("Could not save Spotify sign-in in system credential custody".into())
            })
    }

    pub(super) fn playback_account(&self, sign_in: bool) -> Result<String, BackendError> {
        let profile = self.get("me", &[])?;
        // This resolves identity/grants, not playback eligibility. The native
        // session must confirm Premium before constructing any audio output.
        // Deprecated/missing Web API metadata is neither Premium nor Free.
        if matches!(
            profile.get("product").and_then(Value::as_str),
            Some("free" | "open")
        ) {
            return Err(BackendError(
                "Spotify Premium is required for native playback; you can still browse your music"
                    .into(),
            ));
        }
        let account = profile
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| BackendError("Spotify did not confirm your account identity".into()))?
            .to_string();
        #[cfg(feature = "spotify-native")]
        {
            use crate::audio::spotify_audio::{STREAMING_ACCOUNT, STREAMING_USE};
            if sign_in {
                self.sign_in_for(STREAMING_USE, STREAMING_ACCOUNT)?;
            }
            let streaming_profile = self.get_for(STREAMING_USE, STREAMING_ACCOUNT, "me", &[])?;
            if matches!(
                streaming_profile.get("product").and_then(Value::as_str),
                Some("free" | "open")
            ) {
                return Err(BackendError(
                    "Spotify Premium is required for native playback".into(),
                ));
            }
            let stable_identity = match (
                profile.get("account_id"),
                streaming_profile.get("account_id"),
            ) {
                (None, None) => true,
                (Some(a), Some(b)) => a
                    .as_str()
                    .is_some_and(|a| !a.is_empty() && b.as_str() == Some(a)),
                _ => false,
            };
            if !stable_identity
                || streaming_profile.get("id").and_then(Value::as_str) != Some(account.as_str())
            {
                return Err(BackendError(
                    "Catalogue and playback are signed in to different Spotify accounts".into(),
                ));
            }
            Ok(account)
        }
        #[cfg(not(feature = "spotify-native"))]
        {
            let _ = (sign_in, account);
            Err(BackendError(
                "This build does not include native Spotify playback".into(),
            ))
        }
    }

    fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<Value, BackendError> {
        self.get_for(USE, ACCOUNT, path, params)
    }

    fn get_for(
        &self,
        usage: OAuthUse,
        account: &str,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, BackendError> {
        self.request_for(usage, account, &reqwest::Method::GET, path, params, None)
    }

    fn request_for(
        &self,
        usage: OAuthUse,
        account: &str,
        method: &reqwest::Method,
        path: &str,
        params: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, BackendError> {
        if self
            .retry_after
            .lock()
            .map_err(|_| BackendError("Spotify retry state unavailable".into()))?
            .is_some_and(|until| ui::time::now() < until)
        {
            return Err(BackendError(
                "Spotify is rate limited. Wait before retrying.".into(),
            ));
        }
        for force in [false, true] {
            let token = self
                .custody
                .oauth_access_token(&AccountId::from(account.to_string()), usage, force)
                .map_err(|_| BackendError("Connect Spotify again to browse your library".into()))?;
            let mut endpoint = url::Url::parse(&format!("{}{path}", self.api_origin))
                .map_err(|_| BackendError("Invalid Spotify endpoint".into()))?;
            if !params.is_empty() {
                endpoint
                    .query_pairs_mut()
                    .extend_pairs(params.iter().copied());
            }
            let mut request = self
                .http
                .request(method.clone(), endpoint)
                .bearer_auth(token.as_str());
            if let Some(body) = body {
                request = request.json(body);
            } else if *method != reqwest::Method::GET {
                request = request.body(Vec::new());
            }
            let response = request.send().map_err(|_| {
                BackendError("Spotify is unreachable. Check your connection and retry.".into())
            })?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED && !force {
                continue;
            }
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let seconds = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(60)
                    .min(86400);
                *self
                    .retry_after
                    .lock()
                    .map_err(|_| BackendError("Spotify retry state unavailable".into()))? =
                    ui::time::now().checked_add(Duration::from_secs(seconds));
            }
            if let Some(error) = status_error(response.status().as_u16()) {
                return Err(error);
            }
            if response.status() == reqwest::StatusCode::NO_CONTENT {
                return Ok(Value::Null);
            }
            let mut body = Vec::new();
            response
                .take(MAX_RESPONSE + 1)
                .read_to_end(&mut body)
                .map_err(|_| BackendError("Spotify response could not be read".into()))?;
            if body.len() as u64 > MAX_RESPONSE {
                return Err(BackendError("Spotify response is too large".into()));
            }
            if body.is_empty() {
                return Ok(Value::Null);
            }
            return serde_json::from_slice(&body)
                .map_err(|_| BackendError("Spotify returned an invalid response".into()));
        }
        Err(BackendError(
            "Spotify sign-in expired. Connect again.".into(),
        ))
    }

    /// Bound response and catalogue size without accepting server-provided URLs.
    fn pages(&self, path: &str) -> Result<Vec<Value>, BackendError> {
        let mut result = Vec::new();
        for page in 0..40 {
            let offset = (page * 50).to_string();
            let value = self.get(path, &[("limit", "50"), ("offset", &offset)])?;
            let entries = value.get("items").unwrap_or(&Value::Null).as_array().ok_or_else(|| BackendError("Spotify did not provide this collection. It may require ownership or collaboration.".into()))?;
            result.extend(entries.iter().cloned());
            if value.get("next").unwrap_or(&Value::Null).is_null() {
                return Ok(result);
            }
            if entries.is_empty() {
                return Err(BackendError(
                    "Spotify returned an incomplete collection".into(),
                ));
            }
        }
        Err(BackendError(
            "This Spotify collection exceeds Tilo's current 2,000-item limit".into(),
        ))
    }

    fn search(&self, query: &str, kind: &str) -> Result<SearchBundle, BackendError> {
        let value = self.get("search", &[("q", query), ("type", kind), ("limit", "10")])?;
        for requested in kind.split(',') {
            if !value
                .get(format!("{requested}s"))
                .unwrap_or(&Value::Null)
                .get("items")
                .unwrap_or(&Value::Null)
                .is_array()
            {
                return Err(BackendError(
                    "Spotify returned incomplete search results".into(),
                ));
            }
        }
        let tracks = items(value.get("tracks").unwrap_or(&Value::Null))
            .iter()
            .filter_map(track)
            .collect::<Vec<_>>();
        let albums: Vec<AlbumHit> = items(value.get("albums").unwrap_or(&Value::Null))
            .iter()
            .filter_map(album)
            .collect();
        let artists: Vec<ArtistHit> = items(value.get("artists").unwrap_or(&Value::Null))
            .iter()
            .filter_map(artist)
            .collect();
        let playlists: Vec<PlaylistHit> = items(value.get("playlists").unwrap_or(&Value::Null))
            .iter()
            .filter_map(playlist)
            .collect();
        let top = tracks
            .first()
            .cloned()
            .map(SearchTop::Track)
            .or_else(|| albums.first().cloned().map(SearchTop::Album))
            .or_else(|| artists.first().cloned().map(SearchTop::Artist))
            .or_else(|| playlists.first().cloned().map(SearchTop::Playlist));
        Ok(SearchBundle {
            top,
            tracks,
            albums,
            artists,
            playlists,
        })
    }
}

pub(super) fn status_error(status: u16) -> Option<BackendError> {
    let message = match status {
        200..=299 => return None,
        401 => "Spotify sign-in expired. Connect again.",
        403 => "Spotify denied access. Check account eligibility and Tilo's app allowlist.",
        404 => "This Spotify item is unavailable in your account or region.",
        429 => "Spotify is rate limited. Wait before retrying.",
        _ => "Spotify could not complete the request. Try again later.",
    };
    Some(BackendError(message.into()))
}

pub(super) fn raw_id<'a>(id: &'a str, kind: &str) -> Result<&'a str, BackendError> {
    let prefix = format!("spotify:{kind}:");
    id.strip_prefix(&prefix)
        .filter(|id| id.len() == 22 && id.bytes().all(|byte| byte.is_ascii_alphanumeric()))
        .ok_or_else(|| BackendError("Invalid Spotify resource identifier".into()))
}

fn entity_id(value: &Value, kind: &str) -> Option<String> {
    let id = value.get("id").unwrap_or(&Value::Null).as_str()?;
    let full = format!("spotify:{kind}:{id}");
    raw_id(&full, kind).ok()?;
    Some(full)
}
fn text(value: &Value, field: &str) -> String {
    value
        .get(field)
        .unwrap_or(&Value::Null)
        .as_str()
        .unwrap_or_default()
        .to_owned()
}
fn items(value: &Value) -> &[Value] {
    value
        .get("items")
        .unwrap_or(&Value::Null)
        .as_array()
        .map_or(&[], Vec::as_slice)
}
fn first_artist(value: &Value) -> &Value {
    value
        .get("artists")
        .unwrap_or(&Value::Null)
        .get(0)
        .unwrap_or(&Value::Null)
}
fn cover(value: &Value) -> String {
    value
        .get("images")
        .unwrap_or(&Value::Null)
        .get(0)
        .map_or_else(String::new, |image| text(image, "url"))
}
fn year(value: &Value) -> u32 {
    text(value, "release_date")
        .split('-')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}
fn track(value: &Value) -> Option<TrackHit> {
    if value.get("is_local").unwrap_or(&Value::Null).as_bool() == Some(true)
        || value.get("is_playable").unwrap_or(&Value::Null).as_bool() == Some(false)
    {
        return None;
    }
    let artist = first_artist(value);
    Some(TrackHit {
        video_id: entity_id(value, "track")?,
        title: text(value, "name"),
        artist: text(artist, "name"),
        artist_id: entity_id(artist, "artist"),
        album: value
            .get("album")
            .unwrap_or(&Value::Null)
            .get("name")
            .unwrap_or(&Value::Null)
            .as_str()
            .map(str::to_owned),
        album_id: entity_id(value.get("album").unwrap_or(&Value::Null), "album"),
        duration_s: (value
            .get("duration_ms")
            .unwrap_or(&Value::Null)
            .as_u64()
            .unwrap_or(0)
            / 1000)
            .min(u64::from(u32::MAX)) as u32,
        cover_url: cover(value.get("album").unwrap_or(&Value::Null)),
    })
}
fn album(value: &Value) -> Option<AlbumHit> {
    let artist = first_artist(value);
    Some(AlbumHit {
        id: entity_id(value, "album")?,
        title: text(value, "name"),
        artist: text(artist, "name"),
        artist_id: entity_id(artist, "artist"),
        year: year(value),
        kind: if text(value, "album_type") == "single" {
            AlbumKind::Single
        } else {
            AlbumKind::Album
        },
        cover_url: cover(value),
    })
}
fn artist(value: &Value) -> Option<ArtistHit> {
    Some(ArtistHit {
        id: entity_id(value, "artist")?,
        name: text(value, "name"),
        subscriber_count: 0,
        avatar_url: cover(value),
    })
}
fn playlist(value: &Value) -> Option<PlaylistHit> {
    Some(PlaylistHit {
        id: entity_id(value, "playlist")?,
        title: text(value, "name"),
        subtitle: text(value.get("owner").unwrap_or(&Value::Null), "display_name"),
        track_count: value
            .get("items")
            .unwrap_or(&Value::Null)
            .get("total")
            .unwrap_or(&Value::Null)
            .as_u64()
            .or_else(|| {
                value
                    .get("tracks")
                    .unwrap_or(&Value::Null)
                    .get("total")
                    .unwrap_or(&Value::Null)
                    .as_u64()
            })
            .unwrap_or(0),
        cover_url: cover(value),
    })
}

impl MusicBackend for SpotifyBackend {
    fn name(&self) -> &'static str {
        "Spotify"
    }
    fn is_live(&self) -> bool {
        true
    }
    fn search_tracks(&self, query: &str) -> SearchOutcome {
        Ok(self.search(query, "track")?.tracks)
    }
    fn search_all(&self, query: &str) -> Result<SearchBundle, BackendError> {
        self.search(query, "track,album,artist,playlist")
    }
    fn resolve_audio(&self, _id: &str) -> ResolveOutcome {
        Err(BackendError("Spotify playback is not connected in Tilo. Spotify's Web API does not supply audio streams. Native playback needs a separate streaming session and Premium.".into()))
    }
    fn home(&self) -> Result<HomeContent, BackendError> {
        let reply = self.operation(&crate::backend::SpotifyOperation::LibraryPage {
            offset: 0,
            artists_after: None,
        })?;
        serde_json::from_value(reply.state.get("home_page").unwrap_or(&Value::Null).clone())
            .map_err(|_| BackendError("Spotify library page could not be decoded".into()))
    }
    fn album(&self, id: &str) -> Result<AlbumPage, BackendError> {
        let value = self.get(&format!("albums/{}", raw_id(id, "album")?), &[])?;
        let artist = first_artist(&value);
        let entries = self.pages(&format!("albums/{}/tracks", raw_id(id, "album")?))?;
        let mut metadata = value.clone();
        if let Some(object) = metadata.as_object_mut() {
            object.remove("tracks");
        }
        let tracks = entries
            .iter()
            .filter_map(|track_value| {
                let mut track_value = track_value.clone();
                track_value
                    .as_object_mut()?
                    .insert("album".into(), metadata.clone());
                track(&track_value)
            })
            .collect();
        Ok(AlbumPage {
            id: id.into(),
            title: text(&value, "name"),
            artist_id: entity_id(artist, "artist").unwrap_or_default(),
            artist_name: text(artist, "name"),
            year: year(&value),
            cover_url: cover(&value),
            tracks,
        })
    }
    fn playlist(&self, id: &str) -> Result<PlaylistPage, BackendError> {
        if id == "spotify:collection:liked" {
            let tracks = self
                .pages("me/tracks")?
                .iter()
                .filter_map(|item| item.get("track").and_then(track))
                .collect();
            return Ok(PlaylistPage {
                id: id.into(),
                title: crate::i18n::t("music-spotify-liked"),
                tracks,
                ..Default::default()
            });
        }
        let value = self.get(&format!("playlists/{}", raw_id(id, "playlist")?), &[])?;
        let entries = self.pages(&format!("playlists/{}/items", raw_id(id, "playlist")?))?;
        Ok(PlaylistPage {
            id: id.into(),
            title: text(&value, "name"),
            subtitle: text(value.get("owner").unwrap_or(&Value::Null), "display_name"),
            description: crate::i18n::t("music-spotify-ownership-help"),
            cover_url: cover(&value),
            tracks: entries
                .iter()
                .filter_map(|item| {
                    track(if item.get("item").unwrap_or(&Value::Null).is_object() {
                        item.get("item").unwrap_or(&Value::Null)
                    } else {
                        item.get("track").unwrap_or(&Value::Null)
                    })
                })
                .collect(),
        })
    }
    fn artist(&self, id: &str) -> Result<ArtistPage, BackendError> {
        let value = self.get(&format!("artists/{}", raw_id(id, "artist")?), &[])?;
        let followed = self
            .get("me/library/contains", &[("uris", id)])?
            .as_array()
            .and_then(|items| items.first())
            .and_then(Value::as_bool)
            .ok_or_else(|| BackendError("Spotify did not confirm artist follow status".into()))?;
        let albums = self
            .pages(&format!("artists/{}/albums", raw_id(id, "artist")?))?
            .iter()
            .filter_map(album)
            .map(|album| ArtistAlbum {
                id: album.id,
                title: album.title,
                year: album.year,
                kind: album.kind,
                cover_url: album.cover_url,
            })
            .collect();
        Ok(ArtistPage {
            id: id.into(),
            name: text(&value, "name"),
            header_url: cover(&value),
            albums,
            provider_followed: Some(followed),
            ..Default::default()
        })
    }
}

fn library_home(albums: &Value, playlists: &Value, artists: &Value) -> HomeContent {
    let album_items = items(albums)
        .iter()
        .filter_map(|item| album(item.get("album").unwrap_or(&Value::Null)))
        .map(|album| HomeItem {
            id: album.id,
            title: album.title,
            subtitle: album.artist,
            cover_url: album.cover_url,
            kind: HomeItemKind::Album,
            video_id: String::new(),
            artist_id: String::new(),
            artist_name: String::new(),
            album_id: String::new(),
            album_title: String::new(),
            duration_s: 0,
        })
        .collect();
    let playlist_items = items(playlists)
        .iter()
        .filter_map(playlist)
        .map(|playlist| HomeItem {
            id: playlist.id,
            title: playlist.title,
            subtitle: playlist.subtitle,
            cover_url: playlist.cover_url,
            kind: HomeItemKind::Playlist,
            video_id: String::new(),
            artist_id: String::new(),
            artist_name: String::new(),
            album_id: String::new(),
            album_title: String::new(),
            duration_s: 0,
        })
        .collect();
    let artist_items = items(artists)
        .iter()
        .filter_map(artist)
        .map(|artist| HomeItem {
            id: artist.id,
            title: artist.name,
            subtitle: crate::i18n::t("music-spotify-following"),
            cover_url: artist.avatar_url,
            kind: HomeItemKind::Artist,
            video_id: String::new(),
            artist_id: String::new(),
            artist_name: String::new(),
            album_id: String::new(),
            album_title: String::new(),
            duration_s: 0,
        })
        .collect();
    HomeContent {
        sections: vec![
            HomeSection {
                title: crate::i18n::t("music-spotify-library"),
                items: vec![HomeItem {
                    id: "spotify:collection:liked".into(),
                    title: crate::i18n::t("music-spotify-liked"),
                    subtitle: crate::i18n::t("music-spotify-liked-description"),
                    cover_url: String::new(),
                    kind: HomeItemKind::Playlist,
                    video_id: String::new(),
                    artist_id: String::new(),
                    artist_name: String::new(),
                    album_id: String::new(),
                    album_title: String::new(),
                    duration_s: 0,
                }],
            },
            HomeSection {
                title: crate::i18n::t("music-spotify-albums"),
                items: album_items,
            },
            HomeSection {
                title: crate::i18n::t("music-spotify-playlists"),
                items: playlist_items,
            },
            HomeSection {
                title: crate::i18n::t("music-spotify-artists"),
                items: artist_items,
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_spotify::custody as account_custody;
    fn fixture(
        origin: &str,
        server: &tilo_oauth::fake::FakeAuthorizationServer,
    ) -> (SpotifyBackend, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut table = account_custody::ClientTable::default();
        table.spotify.client_id = "tilo-test".into();
        table.spotify.scopes.music = vec!["user-library-read".into()];
        table.spotify.scopes.music_streaming = vec!["streaming".into(), "user-read-private".into()];
        let custody = Custodian::for_root(Some(root.path().to_owned()))
            .with_client_table(table)
            .with_issuer_endpoints(OAuthIssuer::Spotify, server.provider());
        server.accept_refresh_token("refresh-test");
        custody
            .seal_oauth(
                &AccountId::from(ACCOUNT.to_string()),
                &account_custody::TokenSet {
                    access_token: "initial-test".into(),
                    refresh_token: "refresh-test".into(),
                    expires_in: 3600,
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
        let mut backend = SpotifyBackend::new().unwrap();
        backend.custody = custody;
        backend.api_origin = format!("{origin}/");
        (backend, root)
    }

    #[test]
    #[cfg(feature = "spotify-native")]
    fn spotify_streaming_identity_must_match_the_catalogue_account() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"id":"catalogue","product":"premium"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"different"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"catalogue","product":"premium"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"catalogue"}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        backend
            .custody
            .seal_oauth(
                &AccountId::from(tilo_spotify::STREAMING_ACCOUNT.to_string()),
                &account_custody::TokenSet {
                    access_token: "stream-test".into(),
                    refresh_token: "refresh-test".into(),
                    expires_in: 3600,
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
        assert!(
            backend
                .playback_account(false)
                .unwrap_err()
                .to_string()
                .contains("different Spotify accounts")
        );
        assert_eq!(backend.playback_account(false).unwrap(), "catalogue");
        assert_eq!(
            api.next_request().header("Authorization"),
            Some("Bearer initial-test")
        );
        assert_eq!(
            api.next_request().header("Authorization"),
            Some("Bearer stream-test")
        );
        api.next_request();
        api.next_request();
        assert_eq!(oauth.refreshes(), 0);
    }

    fn account_status_fixture(
        origin: &str,
        oauth: &tilo_oauth::fake::FakeAuthorizationServer,
    ) -> (SpotifyBackend, tempfile::TempDir) {
        let (backend, root) = fixture(origin, oauth);
        for (account, access_token, scope) in [
            (
                ACCOUNT,
                "initial-test",
                "user-read-private user-library-read",
            ),
            (
                tilo_spotify::STREAMING_ACCOUNT,
                "stream-test",
                "streaming user-read-private",
            ),
        ] {
            backend
                .custody
                .seal_oauth(
                    &AccountId::from(account.to_string()),
                    &account_custody::TokenSet {
                        access_token: access_token.into(),
                        refresh_token: "refresh-test".into(),
                        scope: Some(scope.into()),
                        expires_in: 3600,
                        ..Default::default()
                    }
                    .stamp_now(),
                )
                .unwrap();
        }
        (backend, root)
    }

    #[test]
    fn spotify_account_status_reads_both_existing_grants_without_profile_leakage() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        use ui::tilo_resource::Effect as _;
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        for (catalogue, streaming, expected) in [
            (
                serde_json::json!({"id":"private-account", "account_id":"private-stable", "product":"premium", "email":"private-email", "display_name":"private-name"}),
                serde_json::json!({"id":"private-account", "account_id":"private-stable", "product":"premium"}),
                serde_json::json!({"catalogue_product":"premium","streaming_product":"premium","account_match":true}),
            ),
            (
                serde_json::json!({"id":"private-account", "product":"free"}),
                serde_json::json!({"id":"other-private-account", "product":"open"}),
                serde_json::json!({"catalogue_product":"free","streaming_product":"open","account_match":false}),
            ),
            (
                serde_json::json!({"id":"private-account"}),
                serde_json::json!({"id":"private-account", "product":"private-unrecognized-product"}),
                serde_json::json!({"catalogue_product":"unknown","streaming_product":"unknown","account_match":true}),
            ),
            (
                serde_json::json!({"id":"private-account", "account_id":"private-stable", "product":"premium"}),
                serde_json::json!({"id":"private-account", "account_id":"other-private-stable", "product":"premium"}),
                serde_json::json!({"catalogue_product":"premium","streaming_product":"premium","account_match":false}),
            ),
            (
                serde_json::json!({"product":"premium"}),
                serde_json::json!({"product":"premium"}),
                serde_json::json!({"catalogue_product":"premium","streaming_product":"premium","account_match":false}),
            ),
        ] {
            let api = Responder::start(vec![
                Reply::status(200).body(serde_json::to_vec(&catalogue).unwrap()),
                Reply::status(200).body(serde_json::to_vec(&streaming).unwrap()),
            ]);
            let (backend, _root) = account_status_fixture(&api.url(), &oauth);
            let reply = backend.operation(&SpotifyOperation::AccountStatus).unwrap();
            assert_eq!(reply.state, expected);
            assert!(reply.devices.is_empty());
            crate::actions::MusicEffect::SpotifyAccountStatus
                .bound()
                .contract
                .result
                .validate(&tilo_actions::ActionOutput {
                    value: Some(reply.state.clone()),
                    resources: Vec::new(),
                })
                .expect("the real provider result must satisfy the published diagnostic contract");
            let text = reply.state.to_string();
            for forbidden in [
                "private-account",
                "private-stable",
                "private-email",
                "private-name",
                "private-unrecognized-product",
                "initial-test",
                "stream-test",
                "refresh-test",
            ] {
                assert!(!text.contains(forbidden));
            }
            for bearer in ["Bearer initial-test", "Bearer stream-test"] {
                let request = api.next_request();
                assert_eq!(request.method, "GET");
                assert_eq!(request.target, "/me");
                assert!(request.body.is_empty());
                assert_eq!(request.header("Authorization"), Some(bearer));
            }
        }
        assert_eq!(oauth.refreshes(), 0);
        assert!(oauth.requests().is_empty());
    }

    #[test]
    fn spotify_account_status_refusal_never_exposes_provider_body_or_retries() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"id":"private-account","product":"premium"}"#.to_vec()),
            Reply::status(403)
                .body(br#"{"error":"private-secret-refusal","email":"private-email"}"#.to_vec()),
        ]);
        let (backend, _root) = account_status_fixture(&api.url(), &oauth);
        let error = backend
            .operation(&SpotifyOperation::AccountStatus)
            .unwrap_err()
            .to_string();
        assert!(error.contains("denied"));
        for forbidden in [
            "private-secret",
            "private-email",
            "private-account",
            "initial-test",
            "stream-test",
            "refresh-test",
        ] {
            assert!(!error.contains(forbidden));
        }
        for bearer in ["Bearer initial-test", "Bearer stream-test"] {
            let request = api.next_request();
            assert_eq!(request.method, "GET");
            assert_eq!(request.target, "/me");
            assert_eq!(request.header("Authorization"), Some(bearer));
        }
        assert_eq!(oauth.refreshes(), 0);
        assert!(oauth.requests().is_empty());
    }

    #[test]
    fn spotify_account_status_missing_grant_or_scope_never_starts_sign_in() {
        use crate::backend::SpotifyOperation;
        use test_http::Responder;
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(Vec::new());
        let (backend, _root) = account_status_fixture(&api.url(), &oauth);
        backend
            .custody
            .forget_oauth(&AccountId::from(
                tilo_spotify::STREAMING_ACCOUNT.to_string(),
            ))
            .unwrap();
        assert_eq!(
            backend
                .operation(&SpotifyOperation::AccountStatus)
                .unwrap_err()
                .to_string(),
            "Spotify account diagnostic authorization is unavailable"
        );
        for scope in [None, Some("user-read-private".into())] {
            backend
                .custody
                .seal_oauth(
                    &AccountId::from(tilo_spotify::STREAMING_ACCOUNT.to_string()),
                    &account_custody::TokenSet {
                        access_token: "stream-test".into(),
                        refresh_token: "refresh-test".into(),
                        expires_in: 3600,
                        scope,
                        ..Default::default()
                    }
                    .stamp_now(),
                )
                .unwrap();
            let error = backend
                .operation(&SpotifyOperation::AccountStatus)
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("Spotify account diagnostic permissions"));
            assert!(!error.contains("stream-test"));
        }
        assert_eq!(oauth.refreshes(), 0);
        assert!(oauth.requests().is_empty());
        api.assert_untouched();
    }

    #[test]
    fn spotify_connect_confirms_provider_state_and_rejects_acknowledgement_only() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let devices = br#"{"devices":[{"id":"bedroom","name":"Bedroom","is_active":false,"is_restricted":false}]}"#;
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"product":"premium"}"#.to_vec()),
            Reply::status(200).body(devices.to_vec()),
            Reply::status(204),
            Reply::status(200).body(br#"{"device":{"id":"other"},"is_playing":true}"#.to_vec()),
            Reply::status(200).body(br#"{"product":"premium"}"#.to_vec()),
            Reply::status(200).body(devices.to_vec()),
            Reply::status(204),
            Reply::status(200).body(br#"{"device":{"id":"bedroom"},"is_playing":false}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(
            backend
                .operation(&SpotifyOperation::Transfer {
                    device: "bedroom".into()
                })
                .unwrap_err()
                .to_string()
                .contains("not confirmed")
        );
        api.next_request();
        api.next_request();
        let transfer = api.next_request();
        assert_eq!(transfer.method, "PUT");
        assert_eq!(transfer.target, "/me/player");
        assert_eq!(
            serde_json::from_slice::<Value>(&transfer.body).unwrap(),
            serde_json::json!({"device_ids":["bedroom"],"play":false})
        );
        api.next_request();
        let reply = backend
            .operation(&SpotifyOperation::Transfer {
                device: "bedroom".into(),
            })
            .unwrap();
        assert_eq!(reply.state["device"]["id"], "bedroom");
        for _ in 0..4 {
            api.next_request();
        }
        api.assert_untouched();
    }

    #[test]
    fn spotify_connect_unknown_metadata_requires_confirmed_provider_capability() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let track = "spotify:track:1234567890123456789012";
        for profile in [
            serde_json::json!({"id":"account"}),
            serde_json::json!({"id":"account","product":null}),
        ] {
            for operation in [
                SpotifyOperation::Transfer {
                    device: "bedroom".into(),
                },
                SpotifyOperation::RemotePlay {
                    device: "bedroom".into(),
                    track: track.into(),
                },
                SpotifyOperation::RemotePause {
                    device: "bedroom".into(),
                },
            ] {
                let playing = matches!(operation, SpotifyOperation::RemotePlay { .. });
                let state = serde_json::json!({"device":{"id":"bedroom"},"is_playing":playing,"item":{"uri":track}});
                let api = Responder::start(vec![
                    Reply::status(200).body(serde_json::to_vec(&profile).unwrap()),
                    Reply::status(200).body(
                        br#"{"devices":[{"id":"bedroom","name":"Bedroom","is_restricted":false}]}"#
                            .to_vec(),
                    ),
                    Reply::status(204),
                    Reply::status(200).body(serde_json::to_vec(&state).unwrap()),
                ]);
                let (backend, _root) = fixture(&api.url(), &oauth);
                let reply = backend.operation(&operation).unwrap();
                assert_eq!(reply.state, state);
                assert!(
                    reply.state.get("product").is_none(),
                    "no Premium tier is invented"
                );
                assert_eq!(api.next_request().target, "/me");
                assert_eq!(api.next_request().target, "/me/player/devices");
                let command = api.next_request();
                assert_eq!(command.method, "PUT");
                assert!(command.target.starts_with("/me/player"));
                assert_eq!(api.next_request().target, "/me/player");
                api.assert_untouched();
            }
        }
    }

    #[test]
    fn spotify_connect_unknown_metadata_rejects_restricted_devices_before_a_command() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"id":"account"}"#.to_vec()),
            Reply::status(200).body(
                br#"{"devices":[{"id":"restricted","name":"Unavailable","is_restricted":true}]}"#
                    .to_vec(),
            ),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(
            backend
                .operation(&SpotifyOperation::Transfer {
                    device: "restricted".into()
                })
                .unwrap_err()
                .to_string()
                .contains("does not permit remote controls")
        );
        assert_eq!(api.next_request().target, "/me");
        assert_eq!(api.next_request().target, "/me/player/devices");
        api.assert_untouched();
    }

    #[test]
    fn spotify_connect_unknown_metadata_does_not_turn_denial_or_acknowledgement_into_success() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        for status in [403, 500, 204] {
            let mut replies = vec![
                Reply::status(200).body(br#"{"id":"account"}"#.to_vec()),
                Reply::status(200).body(
                    br#"{"devices":[{"id":"bedroom","name":"Bedroom","is_restricted":false}]}"#
                        .to_vec(),
                ),
                Reply::status(status),
            ];
            if status == 204 {
                replies.push(
                    Reply::status(200)
                        .body(br#"{"device":{"id":"other"},"is_playing":true}"#.to_vec()),
                );
            }
            let api = Responder::start(replies);
            let (backend, _root) = fixture(&api.url(), &oauth);
            let error = backend
                .operation(&SpotifyOperation::RemotePlay {
                    device: "bedroom".into(),
                    track: "spotify:track:1234567890123456789012".into(),
                })
                .unwrap_err()
                .to_string();
            assert!(
                !error.contains("require Premium"),
                "missing metadata is not a Free verdict"
            );
            assert!(if status == 403 {
                error.contains("denied access")
            } else if status == 204 {
                error.contains("not confirmed")
            } else {
                error.contains("Spotify")
            });
            for _ in 0..if status == 204 { 4 } else { 3 } {
                api.next_request();
            }
            api.assert_untouched();
        }
    }

    #[test]
    fn spotify_free_accounts_cannot_start_streaming_or_connect_commands() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"product":"free"}"#.to_vec()),
            Reply::status(200).body(br#"{"product":"free"}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(
            backend
                .playback_account(true)
                .unwrap_err()
                .to_string()
                .contains("Premium")
        );
        assert!(
            backend
                .operation(&SpotifyOperation::Transfer {
                    device: "bedroom".into()
                })
                .unwrap_err()
                .to_string()
                .contains("Premium")
        );
        api.next_request();
        api.next_request();
        api.assert_untouched();
        assert_eq!(oauth.refreshes(), 0);
    }

    #[test]
    fn spotify_library_edits_confirm_saves_and_playlist_revisions() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200),
            Reply::status(200).body(b"[true]".to_vec()),
            Reply::status(200),
            Reply::status(200).body(b"[false]".to_vec()),
            Reply::status(200).body(br#"{"id":"owner"}"#.to_vec()),
            Reply::status(200).body(br#"{"owner":{"id":"owner"}}"#.to_vec()),
            Reply::status(201).body(br#"{"snapshot_id":"revision-a"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"owner"}"#.to_vec()),
            Reply::status(200).body(br#"{"owner":{"id":"owner"}}"#.to_vec()),
            Reply::status(200).body(br#"{"snapshot_id":"revision-b"}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        let track = "spotify:track:1234567890123456789012";
        let playlist = "spotify:playlist:1234567890123456789012";
        assert!(
            backend
                .operation(&SpotifyOperation::Save {
                    uri: "spotify:track:../bad".into(),
                    saved: true
                })
                .is_err()
        );
        assert!(api.drain_requests().is_empty());
        assert_eq!(
            backend
                .operation(&SpotifyOperation::Save {
                    uri: track.into(),
                    saved: true
                })
                .unwrap()
                .state["saved"],
            true
        );
        let save = api.next_request();
        assert_eq!(save.method, "PUT");
        assert!(
            save.target
                .starts_with("/me/library?uris=spotify%3Atrack%3A")
        );
        api.next_request();
        assert_eq!(
            backend
                .operation(&SpotifyOperation::Save {
                    uri: track.into(),
                    saved: false
                })
                .unwrap()
                .state["saved"],
            false
        );
        assert_eq!(api.next_request().method, "DELETE");
        api.next_request();
        backend
            .operation(&SpotifyOperation::PlaylistAdd {
                playlist: playlist.into(),
                track: track.into(),
            })
            .unwrap();
        assert_eq!(api.next_request().target, "/me");
        assert_eq!(api.next_request().method, "GET");
        let added = api.next_request();
        assert_eq!(added.method, "POST");
        assert_eq!(added.target, "/playlists/1234567890123456789012/items");
        assert_eq!(
            serde_json::from_slice::<Value>(&added.body).unwrap(),
            serde_json::json!({"uris":[track]})
        );
        backend
            .operation(&SpotifyOperation::PlaylistRemove {
                playlist: playlist.into(),
                track: track.into(),
            })
            .unwrap();
        assert_eq!(api.next_request().target, "/me");
        assert_eq!(api.next_request().method, "GET");
        let removed = api.next_request();
        assert_eq!(removed.method, "DELETE");
        assert_eq!(
            serde_json::from_slice::<Value>(&removed.body).unwrap(),
            serde_json::json!({"items":[{"uri":track}]})
        );
        api.assert_untouched();
    }

    #[test]
    fn spotify_identity_without_subscription_metadata_does_not_claim_premium() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let profiles = if cfg!(feature = "spotify-native") {
            vec![
                Reply::status(200).body(br#"{"id":"catalogue","account_id":"matched"}"#.to_vec()),
                Reply::status(200).body(br#"{"id":"catalogue","account_id":"matched"}"#.to_vec()),
            ]
        } else {
            vec![Reply::status(200).body(br#"{"id":"catalogue"}"#.to_vec())]
        };
        let api = Responder::start(profiles);
        let (backend, _root) = fixture(&api.url(), &oauth);
        #[cfg(feature = "spotify-native")]
        {
            seal_test_streaming_grant(&backend);
            // This is account identity only. The native output gate separately
            // refuses missing/unknown protocol eligibility before any PCM.
            assert_eq!(backend.playback_account(false).unwrap(), "catalogue");
            assert_eq!(api.next_request().target, "/me");
        }
        #[cfg(not(feature = "spotify-native"))]
        assert!(
            backend
                .playback_account(false)
                .unwrap_err()
                .to_string()
                .contains("does not include native")
        );
        assert_eq!(api.next_request().target, "/me");
        api.assert_untouched();
    }

    #[cfg(feature = "spotify-native")]
    fn seal_test_streaming_grant(backend: &SpotifyBackend) {
        backend
            .custody
            .seal_oauth(
                &AccountId::from(tilo_spotify::STREAMING_ACCOUNT.to_string()),
                &account_custody::TokenSet {
                    access_token: "stream-test".into(),
                    refresh_token: "refresh-test".into(),
                    expires_in: 3600,
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
    }

    #[test]
    #[cfg(feature = "spotify-native")]
    fn spotify_explicit_streaming_profile_free_is_refused_before_audio() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"id":"catalogue"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"catalogue","product":"free"}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        seal_test_streaming_grant(&backend);
        assert!(
            backend
                .playback_account(false)
                .unwrap_err()
                .to_string()
                .contains("Premium")
        );
        for _ in 0..2 {
            assert_eq!(api.next_request().target, "/me");
        }
        api.assert_untouched();
        assert_eq!(oauth.refreshes(), 0);
    }

    #[test]
    #[cfg(feature = "spotify-native")]
    fn spotify_stable_account_identity_must_match_even_when_usernames_match() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200)
                .body(br#"{"id":"same","account_id":"first","product":"premium"}"#.to_vec()),
            Reply::status(200).body(br#"{"id":"same","account_id":"second"}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        backend
            .custody
            .seal_oauth(
                &AccountId::from(tilo_spotify::STREAMING_ACCOUNT.to_string()),
                &account_custody::TokenSet {
                    access_token: "stream-test".into(),
                    refresh_token: "refresh-test".into(),
                    expires_in: 3600,
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
        assert!(
            backend
                .playback_account(false)
                .unwrap_err()
                .to_string()
                .contains("different Spotify accounts")
        );
        for _ in 0..2 {
            assert_eq!(api.next_request().target, "/me");
        }
        api.assert_untouched();
    }

    #[test]
    fn spotify_private_playlist_creation_validates_name_and_readback() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(201)
                .body(br#"{"uri":"spotify:playlist:1234567890123456789012"}"#.to_vec()),
            Reply::status(200).body(br#"{"name":"Evening","public":false}"#.to_vec()),
            Reply::status(201)
                .body(br#"{"uri":"spotify:playlist:1234567890123456789012"}"#.to_vec()),
            Reply::status(503),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(
            backend
                .operation(&SpotifyOperation::CreatePlaylist { name: " ".into() })
                .is_err()
        );
        assert!(api.drain_requests().is_empty());
        let created = backend
            .operation(&SpotifyOperation::CreatePlaylist {
                name: "Evening".into(),
            })
            .unwrap();
        assert_eq!(created.state["created"], true);
        let request = api.next_request();
        assert_eq!(request.method, "POST");
        assert_eq!(request.target, "/me/playlists");
        assert_eq!(
            serde_json::from_slice::<Value>(&request.body).unwrap(),
            serde_json::json!({"name":"Evening","public":false})
        );
        assert_eq!(api.next_request().method, "GET");
        assert!(
            backend
                .operation(&SpotifyOperation::CreatePlaylist {
                    name: "Uncertain".into()
                })
                .is_err()
        );
        assert_eq!(api.next_request().method, "POST");
        assert_eq!(api.next_request().method, "GET");
        api.assert_untouched();
    }

    #[test]
    fn spotify_nonowners_cannot_rename_or_edit_saved_playlists() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"id":"me"}"#.to_vec()),
            Reply::status(200).body(
                br#"{"owner":{"id":"other"},"collaborative":true,"items":{"items":[]}}"#.to_vec(),
            ),
            Reply::status(200).body(br#"{"id":"me"}"#.to_vec()),
            Reply::status(200).body(br#"{"owner":{"id":"other"},"collaborative":false}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        let playlist = "spotify:playlist:1234567890123456789012";
        assert!(
            backend
                .operation(&SpotifyOperation::RenamePlaylist {
                    playlist: playlist.into(),
                    name: "Mine".into()
                })
                .unwrap_err()
                .to_string()
                .contains("owner")
        );
        assert!(
            backend
                .operation(&SpotifyOperation::PlaylistAdd {
                    playlist: playlist.into(),
                    track: "spotify:track:1234567890123456789012".into()
                })
                .unwrap_err()
                .to_string()
                .contains("collaborator")
        );
        for _ in 0..4 {
            assert_eq!(api.next_request().method, "GET");
        }
        api.assert_untouched();
    }

    #[test]
    fn spotify_read_only_library_only_requests_granted_collections() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        backend
            .custody
            .seal_oauth(
                &AccountId::from(ACCOUNT.to_string()),
                &account_custody::TokenSet {
                    access_token: "initial-test".into(),
                    refresh_token: "refresh-test".into(),
                    scope: Some("user-read-private user-library-read".into()),
                    expires_in: 3600,
                    ..Default::default()
                }
                .stamp_now(),
            )
            .unwrap();
        let reply = backend
            .operation(&SpotifyOperation::LibraryPage {
                offset: 0,
                artists_after: None,
            })
            .unwrap();
        assert_eq!(reply.state["has_more"], false);
        let home: HomeContent = serde_json::from_value(reply.state["home_page"].clone()).unwrap();
        assert!(home.sections[3].items.is_empty());
        assert!(api.next_request().target.starts_with("/me/albums?"));
        assert!(home.sections[2].items.is_empty());
        assert!(
            backend
                .operation(&SpotifyOperation::Save {
                    uri: "spotify:track:1234567890123456789012".into(),
                    saved: true,
                })
                .unwrap_err()
                .to_string()
                .contains("has not granted")
        );
        assert!(
            backend
                .operation(&SpotifyOperation::Devices)
                .unwrap_err()
                .to_string()
                .contains("has not granted")
        );
        api.assert_untouched();
    }

    #[test]
    fn spotify_library_pages_and_remote_queue_keep_bounded_independent_requests() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let albums: Vec<_> = (0..50).map(|index| serde_json::json!({"album":{"id":format!("{index:022}"),"name":format!("Album {index}")}})).collect();
        let api = Responder::start(vec![
            Reply::status(200).body(serde_json::to_vec(&serde_json::json!({"items":albums,"next":"https://attacker.invalid/?token"})).unwrap()),
            Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
            Reply::status(200).body(br#"{"artists":{"items":[],"next":null}}"#.to_vec()),
            Reply::status(200).body(serde_json::to_vec(&serde_json::json!({"currently_playing":null,"queue":vec![serde_json::json!({"name":"Remote episode","type":"episode"});70]})).unwrap()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(
            backend
                .operation(&SpotifyOperation::LibraryPage {
                    offset: 2000,
                    artists_after: None
                })
                .is_err()
        );
        assert!(api.drain_requests().is_empty());
        let reply = backend
            .operation(&SpotifyOperation::LibraryPage {
                offset: 50,
                artists_after: None,
            })
            .unwrap();
        let home: HomeContent = serde_json::from_value(reply.state["home_page"].clone()).unwrap();
        assert_eq!(home.sections[1].items.len(), 50);
        assert_eq!(reply.state["has_more"], true);
        for _ in 0..2 {
            assert!(api.next_request().target.contains("offset=50"));
        }
        assert!(api.next_request().target.starts_with("/me/following?"));
        let queue = backend.operation(&SpotifyOperation::Queue).unwrap();
        assert_eq!(
            queue.state["remote_queue"]["queue"]
                .as_array()
                .unwrap()
                .len(),
            50
        );
        assert_eq!(queue.state["truncated"], true);
        let request = api.next_request();
        assert_eq!(request.target, "/me/player/queue");
        assert_eq!(request.method, "GET");
        api.assert_untouched();
    }

    #[test]
    fn spotify_followed_artists_use_cursor_and_confirm_follow_changes() {
        use crate::backend::SpotifyOperation;
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let id = "1234567890123456789012";
        let uri = format!("spotify:artist:{id}");
        let previous = "0000000000000000000001";
        let api = Responder::start(vec![
            Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
            Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
            Reply::status(200).body(serde_json::to_vec(&serde_json::json!({"artists":{"items":[{"id":id,"name":"Artist","images":[{"url":"https://example.invalid/art.jpg"}]}],"next":"https://attacker.invalid/","cursors":{"after":id}}})).unwrap()),
            Reply::status(204), Reply::status(200).body(b"[true]".to_vec()),
            Reply::status(204), Reply::status(200).body(b"[true]".to_vec()),
            Reply::status(204), Reply::status(200).body(b"[false]".to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        let silent = Responder::silent();
        let (rejected, _rejected_root) = fixture(&silent.url(), &oauth);
        assert!(
            rejected
                .operation(&SpotifyOperation::LibraryPage {
                    offset: 50,
                    artists_after: Some("bad/identity".into())
                })
                .is_err()
        );
        silent.assert_untouched();
        let result = backend
            .operation(&SpotifyOperation::LibraryPage {
                offset: 50,
                artists_after: Some(previous.into()),
            })
            .unwrap();
        let home: HomeContent = serde_json::from_value(result.state["home_page"].clone()).unwrap();
        assert_eq!(home.sections[3].items[0].id, uri);
        assert_eq!(home.sections[3].items[0].kind, HomeItemKind::Artist);
        assert_eq!(result.state["has_more"], true);
        for _ in 0..2 {
            assert!(api.next_request().target.contains("offset=50"));
        }
        let cursor = api.next_request();
        assert!(cursor.target.starts_with("/me/following?"));
        assert!(cursor.target.contains(&format!("after={previous}")));
        let saved = backend
            .operation(&SpotifyOperation::Save {
                uri: uri.clone(),
                saved: true,
            })
            .unwrap();
        assert_eq!(saved.state["saved"], true);
        assert_eq!(api.next_request().method, "PUT");
        assert!(
            api.next_request()
                .target
                .starts_with("/me/library/contains?")
        );
        assert!(
            backend
                .operation(&SpotifyOperation::Save {
                    uri: uri.clone(),
                    saved: false
                })
                .is_err(),
            "an unconfirmed unfollow is never reported as successful"
        );
        assert_eq!(api.next_request().method, "DELETE");
        assert_eq!(api.next_request().method, "GET");
        assert_eq!(
            backend
                .operation(&SpotifyOperation::Save { uri, saved: false })
                .unwrap()
                .state["saved"],
            false
        );
        assert_eq!(api.next_request().method, "DELETE");
        assert_eq!(api.next_request().method, "GET");
        api.assert_untouched();
    }

    #[test]
    fn spotify_artist_page_requires_a_provider_follow_status() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let id = "1234567890123456789012";
        let uri = format!("spotify:artist:{id}");
        let api = Responder::start(vec![
            Reply::status(200)
                .body(serde_json::to_vec(&serde_json::json!({"id":id,"name":"Artist"})).unwrap()),
            Reply::status(200).body(b"[true]".to_vec()),
            Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
            Reply::status(200)
                .body(serde_json::to_vec(&serde_json::json!({"id":id,"name":"Artist"})).unwrap()),
            Reply::status(200).body(b"[]".to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert_eq!(backend.artist(&uri).unwrap().provider_followed, Some(true));
        assert!(
            api.next_request()
                .target
                .starts_with(&format!("/artists/{id}"))
        );
        assert!(
            api.next_request()
                .target
                .starts_with("/me/library/contains?")
        );
        assert!(
            api.next_request()
                .target
                .starts_with(&format!("/artists/{id}/albums?"))
        );
        assert!(
            backend
                .artist(&uri)
                .unwrap_err()
                .to_string()
                .contains("did not confirm")
        );
        assert_eq!(api.next_request().method, "GET");
        assert_eq!(api.next_request().method, "GET");
        api.assert_untouched();
    }

    #[test]
    fn spotify_followed_artist_pages_refuse_missing_stalled_or_malformed_cursors() {
        use crate::backend::SpotifyOperation;
        use serde_json::json;
        use test_http::{Reply, Responder};
        let id = "1234567890123456789012";
        for artists in [
            json!({"items":[],"next":"https://attacker.invalid","cursors":{"after":id}}),
            json!({"items":[{"id":id,"name":"Artist"}],"next":"https://attacker.invalid"}),
            json!({"items":[{"id":id,"name":"Artist"}],"next":"https://attacker.invalid","cursors":{"after":id}}),
            json!({"items":[{"id":"bad/artist","name":"Artist"}],"next":null}),
        ] {
            let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
            let api = Responder::start(vec![
                Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
                Reply::status(200).body(br#"{"items":[],"next":null}"#.to_vec()),
                Reply::status(200).body(serde_json::to_vec(&json!({"artists":artists})).unwrap()),
            ]);
            let (backend, _root) = fixture(&api.url(), &oauth);
            assert!(
                backend
                    .operation(&SpotifyOperation::LibraryPage {
                        offset: 50,
                        artists_after: Some(id.into())
                    })
                    .is_err()
            );
            for _ in 0..3 {
                assert_eq!(api.next_request().method, "GET");
            }
            api.assert_untouched();
        }
    }

    #[test]
    fn spotify_refreshes_once_and_rate_limit_blocks_repeat_requests() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        oauth.issue_access_token("renewed-test");
        let api = Responder::start(vec![
            Reply::status(401),
            Reply::status(200).body(br#"{"tracks":{"items":[]}}"#.to_vec()),
            Reply::status(429).header("Retry-After", "300"),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        assert!(backend.search_tracks("test & artist").unwrap().is_empty());
        let first = api.next_request();
        assert_eq!(first.header("Authorization"), Some("Bearer initial-test"));
        assert!(first.target.contains("q=test+%26+artist"));
        assert_eq!(
            api.next_request().header("Authorization"),
            Some("Bearer renewed-test")
        );
        assert_eq!(oauth.refreshes(), 1);
        assert!(
            backend
                .search_tracks("limited")
                .unwrap_err()
                .to_string()
                .contains("rate limited")
        );
        api.next_request();
        assert!(
            backend
                .search_tracks("repeat")
                .unwrap_err()
                .to_string()
                .contains("rate limited")
        );
        assert_eq!(oauth.refreshes(), 1);
        api.assert_untouched();
    }

    #[test]
    fn spotify_revocation_and_disconnect_never_retry_with_a_dead_grant() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![Reply::status(401)]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        oauth.revoke_all_refresh_tokens();
        assert!(backend.search_tracks("revoked").is_err());
        api.next_request();
        assert!(backend.search_tracks("again").is_err());
        api.assert_untouched();
        backend.disconnect().unwrap();
        backend.disconnect().unwrap();
        assert!(backend.check_connected().is_err());
        api.assert_untouched();
    }

    #[test]
    fn spotify_pages_use_bounded_offsets_and_refuse_incomplete_collections() {
        use test_http::{Reply, Responder};
        let oauth = tilo_oauth::fake::FakeAuthorizationServer::start();
        let api = Responder::start(vec![
            Reply::status(200).body(
                br#"{"items":[{"name":"first"}],"next":"https://attacker.invalid/token"}"#.to_vec(),
            ),
            Reply::status(200).body(br#"{"items":[{"name":"second"}],"next":null}"#.to_vec()),
            Reply::status(200).body(br#"{"items":null}"#.to_vec()),
        ]);
        let (backend, _root) = fixture(&api.url(), &oauth);
        let entries = backend.pages("me/albums").unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(text(&entries[0], "name"), "first");
        assert_eq!(text(&entries[1], "name"), "second");
        assert!(api.next_request().target.contains("offset=0"));
        assert!(api.next_request().target.contains("offset=50"));
        assert!(backend.pages("me/albums").is_err());
        api.next_request();
        api.assert_untouched();
    }

    #[test]
    fn spotify_metadata_retains_identity_and_refuses_fabricated_audio() {
        let value = serde_json::json!({"id":"1234567890123456789012", "name":"Track", "duration_ms":123_456,
            "artists":[{"id":"abcdefghijklmnopqrstuv","name":"Artist"}],
            "album":{"id":"ABCDEFGHIJKLMNOPQRSTUV","name":"Album"}});
        let hit = track(&value).unwrap();
        assert_eq!(hit.video_id, "spotify:track:1234567890123456789012");
        assert_eq!(
            hit.artist_id.as_deref(),
            Some("spotify:artist:abcdefghijklmnopqrstuv")
        );
        assert_eq!(hit.duration_s, 123);
        let backend = SpotifyBackend::new().unwrap();
        assert!(
            backend
                .resolve_audio(&hit.video_id)
                .unwrap_err()
                .to_string()
                .contains("Premium")
        );
        for id in [
            "spotify:track:../../me",
            "https://attacker.invalid",
            "spotify:album:1234567890123456789012",
        ] {
            assert!(raw_id(id, "track").is_err());
        }
        assert!(status_error(200).is_none());
        for code in [401, 403, 404, 429, 500] {
            assert!(status_error(code).is_some());
        }
    }
}
