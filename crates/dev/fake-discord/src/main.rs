//! Run a fake Discord on loopback until interrupted, for poking the
//! connector's worker by hand or from an end-to-end journey.
//!
//! `fake-discord [--rest 127.0.0.1:PORT] [--gateway 127.0.0.1:PORT]
//! [--control 127.0.0.1:PORT]` prints the environment a `discord-worker`
//! needs to use it.
//!
//! With `--control`, a script acts as Discord's people over plain HTTP, the
//! way the crate's tests do through [`fake_discord::FakeDiscord`]:
//! - `POST /post?channel=&user=&name=&content=` posts as that user; a
//!   non-empty body is a file, named by `file=` and typed by `type=`;
//!   answers `{"id": ...}`.
//! - `POST /edit?message=&content=` and `POST /delete?message=` change or
//!   remove a message, as its author would.
//! - `GET /messages?channel=` answers the channel's messages, oldest first.
//! - `POST /slash?user=&name=&command=&code=` runs a slash command as that
//!   user in #general (`command=link&code=ABC123`, `command=unlink`), as
//!   Discord delivers it: an interaction on the gateway; answers `{"id": ...}`.
//! - `GET /replies` answers the bot's replies to interactions, oldest first,
//!   each `{"interaction": id, "reply": body}`.

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
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().unwrap_or_default();
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
                    "unknown flag {other}; use --rest, --gateway or --control"
                )));
            }
        }
    }
    let fake = Arc::new(fake_discord::FakeDiscord::start_on(rest, gateway).await?);
    println!("DISCORD_API_BASE={}", fake.api_base());
    println!("DISCORD_CDN_BASE={}", fake.cdn_base());
    println!("DISCORD_ALLOW_LOOPBACK=1");
    println!(
        "# bot token (store it as the connector's bot-token secret): {}",
        fake_discord::TOKEN
    );
    println!("# gateway: {}", fake.gateway_url());
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
    fake: &fake_discord::FakeDiscord,
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
            let user = fake_discord::User::new(&arg("user"), &arg("name"));
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
            let user = fake_discord::User::new(&arg("user"), &arg("name"));
            let code = arg("code");
            let id = fake.slash(
                &user,
                &arg("command"),
                Some(code.as_str()).filter(|c| !c.is_empty()),
            );
            (200, json!({ "id": id }))
        }
        ("GET", "/replies") => (200, json!(fake.interaction_replies())),
        _ => (
            404,
            json!({ "error": "use /post, /edit, /delete, /messages, /slash or /replies" }),
        ),
    };
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(answer.to_string())))
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new())))
}

fn decode(text: &str) -> String {
    percent_encoding::percent_decode_str(&text.replace('+', " "))
        .decode_utf8_lossy()
        .into_owned()
}
