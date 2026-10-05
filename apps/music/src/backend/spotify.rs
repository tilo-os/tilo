//! Spotify's public Web API catalogue. No audio URLs, credentials or DRM
//! material enter the collaborative document. Playback is deliberately refused:
//! a Web API grant is not a native streaming session.
use std::io::Read as _;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use account_custody::{Capability, Custodian, OAuthIssuer, OAuthUse};
use serde_json::Value;

use super::{
    AlbumHit, AlbumKind, AlbumPage, ArtistAlbum, ArtistHit, ArtistPage, BackendError, HomeContent,
    HomeItem, HomeItemKind, HomeSection, MusicBackend, PlaylistHit, PlaylistPage, ResolveOutcome,
    SearchBundle, SearchKind, SearchOutcome, SearchTop, TrackHit,
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
            .forget_oauth(&accounts::AccountId::from(ACCOUNT.to_string()))
            .map_err(|_| BackendError("Could not forget Spotify sign-in".into()))
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
        let authorization = self.custody.begin_sign_in_for(USE, None)
            .map_err(|_| BackendError("Spotify sign-in is not configured or its callback port is busy. Register Tilo's public client with http://127.0.0.1:47654/oauth.".into()))?;
        ui::portal::open_uri(authorization.url()).map_err(|_| {
            BackendError("Could not open Spotify sign-in in the system browser".into())
        })?;
        let tokens = Custodian::finish_sign_in(authorization)
            .map_err(|_| BackendError("Spotify sign-in did not complete. Try again.".into()))?;
        self.custody
            .seal_oauth(&accounts::AccountId::from(ACCOUNT.to_string()), &tokens)
            .map_err(|_| {
                BackendError("Could not save Spotify sign-in in system credential custody".into())
            })
    }

    fn get(&self, path: &str, params: &[(&str, &str)]) -> Result<Value, BackendError> {
        if self
            .retry_after
            .lock()
            .map_err(|_| BackendError("Spotify retry state unavailable".into()))?
            .is_some_and(|until| Instant::now() < until)
        {
            return Err(BackendError(
                "Spotify is rate limited. Wait before retrying.".into(),
            ));
        }
        for force in [false, true] {
            let token = self
                .custody
                .oauth_access_token(&accounts::AccountId::from(ACCOUNT.to_string()), USE, force)
                .map_err(|_| BackendError("Connect Spotify again to browse your library".into()))?;
            let mut endpoint = url::Url::parse(&format!("{}{path}", self.api_origin))
                .map_err(|_| BackendError("Invalid Spotify endpoint".into()))?;
            endpoint
                .query_pairs_mut()
                .extend_pairs(params.iter().copied());
            let response = self
                .http
                .get(endpoint)
                .bearer_auth(token.as_str())
                .send()
                .map_err(|_| {
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
                    Instant::now().checked_add(Duration::from_secs(seconds));
            }
            if let Some(error) = status_error(response.status().as_u16()) {
                return Err(error);
            }
            let mut body = Vec::new();
            response
                .take(MAX_RESPONSE + 1)
                .read_to_end(&mut body)
                .map_err(|_| BackendError("Spotify response could not be read".into()))?;
            if body.len() as u64 > MAX_RESPONSE {
                return Err(BackendError("Spotify response is too large".into()));
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
        "Spotify catalogue • playback unavailable"
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
    fn search_more(&self, query: &str, kind: SearchKind) -> Result<SearchBundle, BackendError> {
        self.search(
            query,
            match kind {
                SearchKind::Songs => "track",
                SearchKind::Albums => "album",
                SearchKind::Artists => "artist",
                SearchKind::Playlists => "playlist",
            },
        )
    }
    fn resolve_audio(&self, _id: &str) -> ResolveOutcome {
        Err(BackendError("Spotify playback is not connected in Tilo. Spotify's Web API does not supply audio streams. Native playback needs a separate streaming session and Premium.".into()))
    }
    fn home(&self) -> Result<HomeContent, BackendError> {
        let albums = self.get("me/albums", &[("limit", "20")])?;
        let playlists = self.get("me/playlists", &[("limit", "20")])?;
        let album_items = items(&albums)
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
        let playlist_items = items(&playlists)
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
        Ok(HomeContent {
            sections: vec![
                HomeSection {
                    title: "Saved Spotify albums".into(),
                    items: album_items,
                },
                HomeSection {
                    title: "Spotify playlists".into(),
                    items: playlist_items,
                },
            ],
        })
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
        let value = self.get(&format!("playlists/{}", raw_id(id, "playlist")?), &[])?;
        let entries = self.pages(&format!("playlists/{}/items", raw_id(id, "playlist")?))?;
        Ok(PlaylistPage {
            id: id.into(),
            title: text(&value, "name"),
            subtitle: text(value.get("owner").unwrap_or(&Value::Null), "display_name"),
            description: String::new(),
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
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(
        origin: &str,
        server: &tilo_oauth::fake::FakeAuthorizationServer,
    ) -> (SpotifyBackend, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let mut table = account_custody::ClientTable::default();
        table.spotify.client_id = "tilo-test".into();
        table.spotify.scopes.music = vec!["user-library-read".into()];
        let custody = Custodian::for_root(Some(root.path().to_owned()))
            .with_client_table(table)
            .with_issuer_endpoints(OAuthIssuer::Spotify, server.provider());
        server.accept_refresh_token("refresh-test");
        custody
            .seal_oauth(
                &accounts::AccountId::from(ACCOUNT.to_string()),
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
