//! Run a fake Discord on loopback until interrupted, for poking the
//! connector's worker by hand or from an end-to-end journey.
//!
//! `fake-discord [--rest 127.0.0.1:PORT] [--gateway 127.0.0.1:PORT]
//! [--control 127.0.0.1:PORT]` prints the environment a `discord-worker`
//! needs to use it.
//!
//! With `--control`, a script acts as Discord's people over plain HTTP, the
//! way the crate's tests do through [`fake_discord::FakeDiscord`]:
//! - `POST /post?channel=&user=&name=&content=&avatar=` posts as that
//!   user (`avatar` an avatar hash, optional); a non-empty body is a file,
//!   named by `file=` and typed by `type=`; answers `{"id": ...}`.
//! - `POST /edit?message=&content=` and `POST /delete?message=` change or
//!   remove a message, as its author would.
//! - `GET /messages?channel=` answers the channel's messages, oldest first.
//! - `POST /slash?user=&name=&command=&avatar=` runs a slash command as
//!   that user in #general (`command=link`, `command=unlink`), as Discord
//!   delivers it: an interaction on the gateway; answers `{"id": ...}`.
//! - `GET /replies` answers the bot's replies to interactions, oldest first,
//!   each `{"interaction": id, "reply": body}`; `GET /edits` the bot's
//!   later edits of them, each `{"interaction": id, "edit": body}`.
//!
//! Signing in with Discord (the REST address serves the consent page at
//! `/oauth2/authorize`, the token endpoint and `/users/@me`):
//! - `POST /signin?user=&name=&avatar=` signs that user in to Discord: the
//!   consent page authorizes their account (default `4242`, `qwolff`).
//! - `POST /cdn?path=avatars/<user>/<hash>.png&type=image/png` with the
//!   bytes as body serves them on the CDN (an avatar a user carries).
//!
//! Reactions and the server's own emoji (`--emojis name:id,...` gives the
//! guild custom emoji from the start, as its `GUILD_CREATE` lists them):
//! - `POST /react?channel=&message=&user=&name=&emoji=` and
//!   `POST /unreact?...` add or take back that user's reaction, as Discord
//!   delivers it; `emoji` is a Unicode emoji, or `:name:` for a custom one
//!   (the guild's of that name, with its id, when it has one).
//! - `GET /reactions?message=` answers the emoji the bot reacted to that
//!   message with, oldest first: Unicode, or `name:id` for a custom one.
//! - `GET /emojis` answers the guild's custom emoji; `POST /emojis?set=
//!   name:id,name:id` replaces them, as a moderator uploading or deleting
//!   one does (`GUILD_EMOJIS_UPDATE`).
//!
//! Voice (the fake voice server runs on loopback; `VOICE_SERVER_UPDATE`
//! names it, so the worker needs nothing beyond `DISCORD_ALLOW_LOOPBACK`):
//! - `POST /voice/join?user=&name=&channel=` puts that user in a voice
//!   channel (default the seeded `voice`); in the bot's channel they join
//!   its call as a DAVE client.
//! - `POST /voice/leave?user=` and `POST /voice/mute?user=&muted=1` take
//!   them out or (un)mute them.
//! - `POST /voice/tone?user=&hz=&seconds=` has them say a sine tone to the
//!   bot in real time (Opus, DAVE and transport encrypted); answers at once.
//! - `GET /voice/heard?hz=440,660&seconds=N` answers what the fake decoded
//!   from the bot's audio in the last N seconds (default 5): `frames`, and
//!   per frequency the share of the energy at it (`{"440": 0.8, ...}`, about
//!   1 for that tone alone, about 0.5 each for two).
//! - `GET /voice/state` answers the bot's voice side: its channel, whether
//!   its voice connection is up, DAVE's version and readiness, its speaking
//!   flags, the channel statuses and nickname it set, who is in voice.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut rest: SocketAddr = ([127, 0, 0, 1], 0).into();
    let mut gateway: SocketAddr = ([127, 0, 0, 1], 0).into();
    let mut control: Option<SocketAddr> = None;
    let mut emojis = String::new();
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
        if flag == "--emojis" {
            emojis = value;
            continue;
        }
        let parsed: SocketAddr = value.parse().map_err(|_| {
            std::io::Error::other(format!(
                "{flag} wants an address like 127.0.0.1:8080, not {value:?}"
            ))
        })?;
        if !parsed.ip().is_loopback() {
            return Err(std::io::Error::other("the fake only listens on loopback"));
        }
        match flag.as_str() {
            "--rest" => rest = parsed,
            "--gateway" => gateway = parsed,
            "--control" => control = Some(parsed),
            other => {
                return Err(std::io::Error::other(format!(
                    "unknown flag {other}; use --rest, --gateway, --control or --emojis"
                )));
            }
        }
    }
    let fake = Arc::new(fake_discord::FakeDiscord::start_on(rest, gateway).await?);
    set_emojis(&fake, &emojis);
    println!("DISCORD_API_BASE={}", fake.api_base());
    println!("DISCORD_CDN_BASE={}", fake.cdn_base());
    println!("DISCORD_ALLOW_LOOPBACK=1");
    println!(
        "# bot token (store it as the connector's bot-token secret): {}",
        fake_discord::TOKEN
    );
    println!("# gateway: {}", fake.gateway_url());
    println!(
        "# voice: ws://{} (named in VOICE_SERVER_UPDATE; DISCORD_ALLOW_LOOPBACK=1 lets the worker reach it)",
        fake.voice_endpoint()
    );
    if let Some(control) = control {
        let listener = tokio::net::TcpListener::bind(control).await?;
        println!("# control: http://{}", listener.local_addr()?);
        tokio::spawn(serve_control(fake, listener));
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}

async fn serve_control(fake: Arc<fake_discord::FakeDiscord>, listener: tokio::net::TcpListener) {
    while let Ok((socket, _)) = listener.accept().await {
        let fake = Arc::clone(&fake);
        tokio::spawn(async move {
            let service = hyper::service::service_fn(move |request| {
                let fake = Arc::clone(&fake);
                async move { Ok::<_, Infallible>(control(&fake, request).await) }
            });
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(hyper_util::rt::TokioIo::new(socket), service)
                .await;
        });
    }
}

async fn control(
    fake: &Arc<fake_discord::FakeDiscord>,
    request: Request<Incoming>,
) -> Response<Full<Bytes>> {
    let method = request.method().as_str().to_owned();
    let path = request.uri().path().to_owned();
    let query: Vec<(String, String)> = request
        .uri()
        .query()
        .unwrap_or_default()
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let arg = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map_or_else(String::new, |(_, v)| v.clone())
    };
    let body = request
        .into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .unwrap_or_default();
    let (status, answer): (u16, Value) = match (method.as_str(), path.as_str()) {
        ("POST", "/post") => {
            let user = person(&arg);
            let id = if body.is_empty() {
                fake.post(&arg("channel"), &user, &arg("content"))
            } else {
                fake.post_file(
                    &arg("channel"),
                    &user,
                    &arg("content"),
                    (&arg("file"), &arg("type"), &body),
                )
            };
            (200, json!({ "id": id }))
        }
        ("POST", "/edit") => {
            fake.edit(&arg("message"), &arg("content"));
            (200, json!({}))
        }
        ("POST", "/delete") => {
            fake.delete(&arg("message"), None);
            (200, json!({}))
        }
        ("GET", "/messages") => (200, json!(fake.messages_in(&arg("channel")))),
        ("POST", "/slash") => {
            let id = fake.slash(&person(&arg), &arg("command"));
            (200, json!({ "id": id }))
        }
        ("GET", "/replies") => (200, json!(fake.interaction_replies())),
        ("GET", "/edits") => (200, json!(fake.interaction_edits())),
        ("POST", "/signin") => {
            fake.sign_in(person(&arg));
            (200, json!({}))
        }
        ("POST", "/cdn") => {
            let url = fake.put_cdn(&arg("path"), &arg("type"), &body);
            (200, json!({ "url": url }))
        }
        ("POST", "/react" | "/unreact") | (_, "/reactions" | "/emojis") => {
            (200, reactions(fake, &method, &path, &arg))
        }
        ("POST", "/voice/join") => {
            let user = fake_discord::User::new(&arg("user"), &arg("name"));
            let channel = Some(arg("channel"))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| fake_discord::CHANNEL_VOICE.to_owned());
            fake.voice_join(&user, &channel);
            (200, json!({}))
        }
        ("POST", "/voice/leave") => {
            fake.voice_leave(&arg("user"));
            (200, json!({}))
        }
        ("POST", "/voice/mute") => {
            fake.voice_mute(&arg("user"), matches!(arg("muted").as_str(), "1" | "true"));
            (200, json!({}))
        }
        ("POST", "/voice/tone") => {
            tone(fake, arg("user"), &arg("hz"), &arg("seconds"));
            (200, json!({}))
        }
        ("GET", "/voice/heard") => (200, heard(fake, &arg("hz"), &arg("seconds"))),
        ("GET", "/voice/state") => (200, fake.voice_state_json()),
        _ => (
            404,
            json!({ "error": "use /post, /edit, /delete, /messages, /slash, /replies, /react, /unreact, /reactions, /emojis or /voice/..." }),
        ),
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(answer.to_string())))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

/// Have `user` say a sine at `hz` for `seconds`, in real time, in the
/// background.
fn tone(fake: &Arc<fake_discord::FakeDiscord>, user: String, hz: &str, seconds: &str) {
    let hz: f32 = hz.parse().unwrap_or(440.0);
    let seconds: f32 = seconds.parse().unwrap_or(2.0);
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=30000 frames"
    )]
    let count = (seconds.clamp(0.0, 600.0) * 50.0).round() as usize;
    let frames = fake_discord::tone::sine(hz, count);
    let fake = Arc::clone(fake);
    tokio::spawn(async move { fake.voice_say(&user, &frames).await });
}

/// What the fake heard from the bot in the last `seconds`, per frequency.
fn heard(fake: &fake_discord::FakeDiscord, hz: &str, seconds: &str) -> Value {
    let seconds: u64 = seconds.parse().unwrap_or(5);
    let since = fake_discord::now_ms().saturating_sub(seconds.saturating_mul(1000));
    let frames: Vec<Vec<u8>> = fake
        .voice_heard_frames()
        .into_iter()
        .filter(|heard| heard.at_ms >= since)
        .map(|heard| heard.opus)
        .collect();
    let samples = fake_discord::tone::decode(&frames);
    let shares: serde_json::Map<String, Value> = hz
        .split(',')
        .filter_map(|f| {
            f.trim()
                .parse::<f32>()
                .ok()
                .map(|hz| (f.trim().to_owned(), hz))
        })
        .map(|(name, hz)| (name, json!(fake_discord::tone::share(&samples, hz))))
        .collect();
    json!({"frames": frames.len(), "share": shares})
}

/// The reaction and emoji routes: a user's reaction added or taken back,
/// the bot's reactions on a message, and the guild's custom emoji.
fn reactions(
    fake: &fake_discord::FakeDiscord,
    method: &str,
    path: &str,
    arg: &dyn Fn(&str) -> String,
) -> Value {
    match (method, path) {
        ("POST", "/react" | "/unreact") => {
            let user = fake_discord::User::new(&arg("user"), &arg("name"));
            let (channel, message, emoji) = (arg("channel"), arg("message"), arg("emoji"));
            if path == "/react" {
                fake.react(&channel, &message, &user, &emoji);
            } else {
                fake.unreact(&channel, &message, &user, &emoji);
            }
            json!({})
        }
        ("POST", "/emojis") => {
            set_emojis(fake, &arg("set"));
            json!(fake.emojis())
        }
        (_, "/emojis") => json!(fake.emojis()),
        _ => json!(fake.bot_reactions_on(&arg("message"))),
    }
}

/// Make the guild's custom emoji `list`, `name:id` comma separated; a
/// name alone gets an id of its own.
fn set_emojis(fake: &fake_discord::FakeDiscord, list: &str) {
    let parsed: Vec<(String, String)> = list
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .enumerate()
        .map(|(index, entry)| match entry.split_once(':') {
            Some((name, id)) => (name.to_owned(), id.to_owned()),
            None => (
                entry.to_owned(),
                5100_usize.saturating_add(index).to_string(),
            ),
        })
        .collect();
    let emojis: Vec<(&str, &str, bool)> = parsed
        .iter()
        .map(|(name, id)| (name.as_str(), id.as_str(), false))
        .collect();
    fake.set_emojis(&emojis);
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}

/// The Discord user a control request names (`user=&name=`, with
/// `avatar=` an avatar hash when they have one).
fn person(arg: &impl Fn(&str) -> String) -> fake_discord::User {
    let user = fake_discord::User::new(&arg("user"), &arg("name"));
    match arg("avatar") {
        hash if hash.is_empty() => user,
        hash => user.with_avatar(&hash),
    }
}
