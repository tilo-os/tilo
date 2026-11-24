//! Music's Spotify library, as it asks Spotify's Web API: the songs and albums a person keeps,
//! the artists they follow, the playlists they make and change, what plays next, and the pages
//! of what they keep. The Web API is a stand-in server on this machine, so each test reads the
//! requests Spotify would have seen, method, path, query and body.
//!
//! A sign-in Spotify will not renew is not an error: it is the answer that the person connects
//! again, and the app shows its button.
#![cfg(feature = "live")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "a failed unwrap or index in a test is its assertion"
)]

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tilo_spotify::AccountId;
use tilo_spotify::ownership::{
    Capability, CredentialStore, OAuthIssuer, OAuthUse, Provider, Refresh, Revocation, TokenSet,
};
use tilolang_music::service::{SpotifyOp, SpotifyReply};
use tilolang_music::spotify::Spotify;

/// One request the stand-in saw, as a test reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    method: String,
    /// The path and query, as the request line carried them.
    target: String,
    /// The bearer the request was authorised with.
    bearer: String,
    body: String,
}

/// A server that answers each request with what `answer` says, and keeps what it saw.
struct Stand {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Stand {
    /// A server on a free port of this machine, answering from `answer`.
    fn start(answer: impl Fn(&Seen) -> (u16, String) + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let mut request = Vec::new();
                let mut buffer = [0u8; 4096];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let head = String::from_utf8_lossy(&request).into_owned();
                let mut lines = head.split("\r\n");
                let mut first = lines.next().unwrap_or_default().split_whitespace();
                let method = first.next().unwrap_or_default().to_owned();
                let target = first.next().unwrap_or_default().to_owned();
                let bearer = lines
                    .find_map(|line| {
                        line.strip_prefix("authorization: Bearer ")
                            .or_else(|| line.strip_prefix("Authorization: Bearer "))
                            .map(str::to_owned)
                    })
                    .unwrap_or_default();
                let length = lines
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                let split = head.find("\r\n\r\n").map_or(request.len(), |at| at + 4);
                while request.len() < split + length {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(&buffer[..read]),
                    }
                }
                let body =
                    String::from_utf8_lossy(request.get(split..).unwrap_or_default()).into_owned();
                let seen = Seen {
                    method,
                    target,
                    bearer,
                    body,
                };
                kept.lock().unwrap().push(seen.clone());
                let (status, payload) = answer(&seen);
                let reason = match status {
                    200 => "OK",
                    204 => "No Content",
                    400 => "Bad Request",
                    401 => "Unauthorized",
                    _ => "Error",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Self { base, seen }
    }

    /// What the server was asked, in order.
    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    /// The targets asked, in order.
    fn targets(&self) -> Vec<String> {
        self.seen().into_iter().map(|seen| seen.target).collect()
    }

    fn api(&self) -> String {
        format!("{}/v1", self.base)
    }
}

/// The use Music's Spotify grant is kept under, as the system's account vault names it.
fn use_() -> OAuthUse {
    OAuthUse::new(OAuthIssuer::Spotify, Capability::Music)
}

/// A signed-in Spotify over the stand-in `base`, and the scratch vault it reads the grant from.
fn signed_in(stand: &Stand) -> (Spotify, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = CredentialStore::for_root(Some(dir.path().join("tilo")));
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    store
        .seal_oauth(
            &AccountId::from("music.spotify"),
            &TokenSet {
                access_token: "at-1".to_owned(),
                refresh_token: "rt-1".to_owned(),
                expires_in: 3_600,
                obtained_at: now,
                scope: None,
            },
        )
        .unwrap();
    (Spotify::over(store, &stand.api()).unwrap(), dir)
}

/// A track as Spotify's API answers one.
fn track(id: &str, title: &str, artist: &str) -> String {
    format!(
        r#"{{"id":"{id}","name":"{title}","artists":[{{"id":"artist-{artist}","name":"{artist}"}}],"album":{{"id":"album-1","name":"Morning","images":[{{"url":"https://img.example/{id}.jpg"}}]}},"duration_ms":214000,"type":"track"}}"#
    )
}

#[test]
fn a_like_puts_the_song_in_the_library_and_reads_back_what_spotify_holds() {
    let stand = Stand::start(|seen| match seen.target.as_str() {
        "/v1/me/tracks/contains?ids=track-1" => (200, "[true]".to_owned()),
        _ => (200, String::new()),
    });
    let (spotify, _vault) = signed_in(&stand);
    let reply = spotify
        .op(&SpotifyOp::Like {
            key: "spotify:track:track-1".to_owned(),
            liked: true,
        })
        .unwrap();
    assert!(
        reply.liked,
        "the song is in the library, as Spotify reads it back"
    );
    assert_eq!(
        stand.seen(),
        vec![
            Seen {
                method: "PUT".to_owned(),
                target: "/v1/me/tracks?ids=track-1".to_owned(),
                bearer: "at-1".to_owned(),
                body: String::new(),
            },
            Seen {
                method: "GET".to_owned(),
                target: "/v1/me/tracks/contains?ids=track-1".to_owned(),
                bearer: "at-1".to_owned(),
                body: String::new(),
            },
        ],
        "the key the app holds is read as the id Spotify's API takes"
    );
}

#[test]
fn an_unlike_takes_it_out_and_an_album_and_an_artist_are_kept_and_followed() {
    let stand = Stand::start(|seen| match seen.target.as_str() {
        "/v1/me/tracks/contains?ids=track-1" => (200, "[false]".to_owned()),
        "/v1/me/albums/contains?ids=album-1" => (200, "[true]".to_owned()),
        "/v1/me/following/contains?type=artist&ids=artist-1" => (200, "[true]".to_owned()),
        _ => (200, String::new()),
    });
    let (spotify, _vault) = signed_in(&stand);
    let unliked = spotify
        .op(&SpotifyOp::Like {
            key: "spotify:track:track-1".to_owned(),
            liked: false,
        })
        .unwrap();
    assert!(!unliked.liked);
    let saved = spotify
        .op(&SpotifyOp::SaveAlbum {
            key: "spotify:album:album-1".to_owned(),
            keep: true,
        })
        .unwrap();
    assert!(saved.saved);
    let followed = spotify
        .op(&SpotifyOp::Follow {
            key: "spotify:artist:artist-1".to_owned(),
            follow: true,
        })
        .unwrap();
    assert!(followed.followed);
    assert_eq!(
        stand.targets(),
        vec![
            "/v1/me/tracks?ids=track-1",
            "/v1/me/tracks/contains?ids=track-1",
            "/v1/me/albums?ids=album-1",
            "/v1/me/albums/contains?ids=album-1",
            "/v1/me/following?type=artist&ids=artist-1",
            "/v1/me/following/contains?type=artist&ids=artist-1",
        ]
    );
    let methods: Vec<String> = stand.seen().into_iter().map(|seen| seen.method).collect();
    assert_eq!(methods[0], "DELETE", "an unlike is the DELETE of the song");
    assert_eq!(methods[2], "PUT", "a saved album is a PUT");
    assert_eq!(methods[4], "PUT", "a followed artist is a PUT");
}

#[test]
fn a_playlist_is_made_renamed_and_given_songs_as_spotify_takes_them() {
    let stand = Stand::start(|seen| {
        match (seen.method.as_str(), seen.target.as_str()) {
        ("POST", "/v1/me/playlists") => (
            200,
            r#"{"id":"pl-1","name":"Roads","description":"","owner":{"display_name":"Ada"},"items":{"total":0}}"#
                .to_owned(),
        ),
        ("GET", "/v1/playlists/pl-1") => (
            200,
            r#"{"id":"pl-1","name":"Roads at night","description":"For driving","owner":{"display_name":"Ada"},"items":{"total":3}}"#
                .to_owned(),
        ),
        _ => (200, String::new()),
    }
    });
    let (spotify, _vault) = signed_in(&stand);
    let made = spotify
        .op(&SpotifyOp::CreatePlaylist {
            name: "Roads".to_owned(),
        })
        .unwrap();
    let made = made.made.expect("the playlist Spotify made");
    assert_eq!(made.key, "spotify:playlist:pl-1");
    assert_eq!(made.title, "Roads");
    assert_eq!(
        stand.seen()[0].body,
        r#"{"name":"Roads","description":"","public":false}"#,
        "a new playlist is private and called what the person typed"
    );
    let edited = spotify
        .op(&SpotifyOp::EditPlaylist {
            key: "spotify:playlist:pl-1".to_owned(),
            name: "Roads at night".to_owned(),
            description: "For driving".to_owned(),
        })
        .unwrap();
    let edited = edited.made.expect("the playlist as Spotify holds it now");
    assert_eq!(edited.title, "Roads at night");
    assert_eq!(edited.description, "For driving");
    assert_eq!(edited.track_count, 3);
    spotify
        .op(&SpotifyOp::AddToPlaylist {
            key: "spotify:playlist:pl-1".to_owned(),
            uri: "spotify:track:track-1".to_owned(),
        })
        .unwrap();
    spotify
        .op(&SpotifyOp::RemoveFromPlaylist {
            key: "spotify:playlist:pl-1".to_owned(),
            uri: "spotify:track:track-1".to_owned(),
        })
        .unwrap();
    let seen = stand.seen();
    assert_eq!(seen[1].method, "PUT");
    assert_eq!(seen[1].target, "/v1/playlists/pl-1");
    assert_eq!(
        seen[1].body,
        r#"{"name":"Roads at night","description":"For driving"}"#
    );
    assert_eq!(seen[2].method, "GET", "the edit is read back");
    assert_eq!(seen[3].method, "POST");
    assert_eq!(seen[3].target, "/v1/playlists/pl-1/items");
    assert_eq!(seen[3].body, r#"{"uris":["spotify:track:track-1"]}"#);
    assert_eq!(seen[5].method, "DELETE");
    assert_eq!(seen[5].target, "/v1/playlists/pl-1/items");
    assert_eq!(
        seen[5].body, r#"{"tracks":[{"uri":"spotify:track:track-1"}]}"#,
        "a removal names the song it takes out"
    );
}

#[test]
fn the_queue_is_read_and_a_song_is_put_after_what_plays() {
    let stand = Stand::start(|seen| match seen.target.as_str() {
        "/v1/me/player/queue?uri=spotify%3Atrack%3Atrack-2" => (204, String::new()),
        _ => (
            200,
            format!(
                r#"{{"currently_playing":{},"queue":[{},{}]}}"#,
                track("track-1", "First Light", "Ava"),
                track("track-1", "First Light", "Ava"),
                track("track-2", "Blue Hour", "Ava")
            ),
        ),
    });
    let (spotify, _vault) = signed_in(&stand);
    let queue = spotify.op(&SpotifyOp::Queue).unwrap();
    assert_eq!(queue.now, "First Light — Ava");
    let titles: Vec<&str> = queue.queue.iter().map(|song| song.title.as_str()).collect();
    assert_eq!(titles, vec!["First Light", "Blue Hour"]);
    assert_eq!(queue.queue[0].key, "spotify:track:track-1");
    assert_eq!(queue.queue[0].album, "Morning");
    let added = spotify
        .op(&SpotifyOp::AddToQueue {
            uri: "spotify:track:track-2".to_owned(),
        })
        .unwrap();
    assert_eq!(
        added.queue.len(),
        2,
        "the queue it went into answers the write"
    );
    assert_eq!(
        stand.targets(),
        vec![
            "/v1/me/player/queue",
            "/v1/me/player/queue?uri=spotify%3Atrack%3Atrack-2",
            "/v1/me/player/queue",
        ]
    );
}

#[test]
fn a_library_page_says_where_the_page_after_it_starts() {
    let stand = Stand::start(|seen| {
        match seen.target.as_str() {
        "/v1/me/tracks?limit=50&offset=50" => (
            200,
            format!(
                r#"{{"items":[{{"track":{}}}],"next":"https://api.spotify.com/v1/me/tracks?offset=100"}}"#,
                track("track-51", "Fifty One", "Ava")
            ),
        ),
        "/v1/me/albums?limit=50&offset=100" => (
            200,
            r#"{"items":[{"album":{"id":"album-9","name":"Late","artists":[{"id":"artist-1","name":"Ava"}],"release_date":"2025-01-01"}}],"next":null}"#
                .to_owned(),
        ),
        _ => (200, String::new()),
    }
    });
    let (spotify, _vault) = signed_in(&stand);
    let page = spotify
        .op(&SpotifyOp::SavedPage {
            kind: "tracks".to_owned(),
            offset: 50,
        })
        .unwrap();
    let page = page
        .pages
        .into_iter()
        .next()
        .expect("a page of liked songs");
    assert_eq!(page.kind, "tracks");
    assert_eq!(page.section.title, "Liked songs");
    assert_eq!(page.section.items.len(), 1);
    assert_eq!(
        page.next, 100,
        "the page after this one starts where Spotify said"
    );
    let last = spotify
        .op(&SpotifyOp::SavedPage {
            kind: "albums".to_owned(),
            offset: 100,
        })
        .unwrap();
    assert_eq!(
        last.pages[0].next, 0,
        "no `next` is the last page, and the app asks for no more"
    );
}

#[test]
fn an_artist_page_publishes_the_genres_spotify_names() {
    let stand = Stand::start(|seen| {
        match seen.target.as_str() {
        "/v1/artists/artist-1" => (
            200,
            r#"{"id":"artist-1","name":"Ava","genres":["dream pop","art pop"],"followers":{"total":120000}}"#
                .to_owned(),
        ),
        "/v1/artists/artist-1/top-tracks?market=from_token" => {
            (200, format!(r#"{{"tracks":[{}]}}"#, track("track-1", "First Light", "Ava")))
        }
        "/v1/artists/artist-1/albums?include_groups=album,single&limit=20" => {
            (200, r#"{"items":[]}"#.to_owned())
        }
        _ => (200, String::new()),
    }
    });
    let (spotify, _vault) = signed_in(&stand);
    let page = spotify
        .op(&SpotifyOp::Artist {
            key: "spotify:artist:artist-1".to_owned(),
        })
        .unwrap();
    let artist = page.artist.expect("the artist's page");
    assert_eq!(artist.artist.name, "Ava");
    assert_eq!(artist.artist.genres, vec!["dream pop", "art pop"]);
    assert_eq!(
        artist.tracks[0].title, "First Light",
        "the key the app holds does not reach Spotify's path as it is"
    );
}

#[test]
fn a_sign_in_spotify_will_not_renew_is_an_answer_that_the_person_connects_again() {
    let stand = Stand::start(|seen| match seen.target.as_str() {
        "/token" => (
            400,
            r#"{"error":"invalid_grant","error_description":"Refresh token revoked"}"#.to_owned(),
        ),
        _ => (
            401,
            r#"{"error":{"message":"The access token expired"}}"#.to_owned(),
        ),
    });
    let dir = tempfile::tempdir().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let store = CredentialStore::for_root(Some(dir.path().join("tilo"))).with_use(
        use_(),
        Provider {
            id: "spotify".to_owned(),
            label: "Spotify".to_owned(),
            authorization_endpoint: format!("{}/authorize", stand.base),
            token_endpoint: format!("{}/token", stand.base),
            revocation: Revocation::Unsupported,
            scopes: Vec::new(),
            authorization_params: Vec::new(),
            refresh: Refresh::RefreshToken,
            device_authorization_endpoint: None,
        },
        "tilo-test-client",
    );
    store
        .seal_oauth(
            &AccountId::from("music.spotify"),
            &TokenSet {
                access_token: "at-1".to_owned(),
                refresh_token: "rt-1".to_owned(),
                expires_in: 3_600,
                obtained_at: now,
                scope: None,
            },
        )
        .unwrap();
    let spotify = Spotify::over(store, &stand.api()).unwrap();
    let reply: SpotifyReply = spotify
        .op(&SpotifyOp::Like {
            key: "spotify:track:track-1".to_owned(),
            liked: true,
        })
        .expect("a sign-in that is over is answered, not failed");
    assert!(
        reply.expired,
        "the app shows its connect button, not an error"
    );
    assert_eq!(
        stand.targets(),
        vec!["/v1/me/tracks?ids=track-1", "/token"],
        "the refused song is met with a renewal, which Spotify refuses: no second try is sent"
    );
}
