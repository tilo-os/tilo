//! Tilo's API served in process, as a key its caller holds, against a Tilo
//! host the caller reaches directly: what the SDKs' native part and the
//! `tilo` command use when there is no API server between them and the host.
//!
//! It is the API server itself ([`Server`]), with one key in its memory: the
//! caller's own, a device of the person's (a computer `tilo login` made one)
//! or a grant of theirs, with the certificate the person's root (or a key of
//! theirs) gave it. Every route is answered as on an API server, by the same
//! code, and the key dials the host as one of the person's devices does; the
//! host decides everything, as it decides for an API server. Nothing here
//! knows what kind of host it reaches.
//!
//! A key is carried as a key file ([`KeyFile`]): its secret and its
//! certificate, hex, and the host it reaches.

use crate::grant_key::GrantKey;
use crate::store::{GrantKind, Store};
use crate::{Body, Config, Reply, Server, now_ms};
use http_body_util::BodyExt as _;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tilo_user_identity::DeviceCert;

/// A key as a file holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyFile {
    /// The Tilo host it reaches: an origin (`https://eu1.tilo.sh`).
    pub host: String,
    /// What the host's people see it as, and History names its runs.
    pub name: String,
    /// The key's 32-byte secret, hex.
    pub key: String,
    /// Its certificate, canonical bytes as hex.
    pub cert: String,
}

impl KeyFile {
    /// `key` with `cert`, reaching `host`, named `name`.
    #[must_use]
    pub fn new(host: &str, name: &str, key: &ed25519_dalek::SigningKey, cert: &DeviceCert) -> Self {
        Self {
            host: host.trim_end_matches('/').to_owned(),
            name: name.to_owned(),
            key: hex::encode(key.to_bytes()),
            cert: hex::encode(
                tilo_user_identity::canonical::canonical_bytes(cert).unwrap_or_default(),
            ),
        }
    }

    /// The key file at `path`.
    ///
    /// # Errors
    /// It cannot be read, or is not a key file.
    pub fn read(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
    }
}

/// Tilo's API in process, as one key.
pub struct Direct {
    server: Arc<Server>,
    authorization: String,
}

impl Direct {
    /// The API as the key `file` holds, reaching its host.
    ///
    /// # Errors
    /// The key or its certificate is not one, or does not hold now.
    pub fn new(file: &KeyFile) -> Result<Self, String> {
        let secret = hex::decode(&file.key).map_err(|_| "the key is not hex".to_owned())?;
        let cert = hex::decode(&file.cert).map_err(|_| "the certificate is not hex".to_owned())?;
        let secret: [u8; 32] = secret
            .try_into()
            .map_err(|_| "a key is 32 bytes".to_owned())?;
        let cert: DeviceCert = tilo_user_identity::canonical::from_canonical_bytes(&cert)
            .map_err(|error| format!("the certificate: {error}"))?;
        let key = GrantKey::held(
            ed25519_dalek::SigningKey::from_bytes(&secret),
            cert,
            now_ms().saturating_mul(1_000_000),
        )
        .map_err(|refused| refused.to_string())?;
        let host = file.host.trim_end_matches('/');
        let server = Server::new(
            &Config {
                issuer: host.to_owned(),
                web_entry: host.to_owned(),
                relay: host.to_owned(),
            },
            Store::in_memory(),
        )
        .map_err(|error| error.to_string())?;
        let (_, token) = server
            .store
            .record_key(&key, &file.name, GrantKind::Key, None, now_ms())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            server,
            authorization: format!("Bearer {token}"),
        })
    }

    /// Answer `method` on `target` (a path with its query) with `headers` and
    /// `body`, as the API server answers it.
    pub async fn request(
        &self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Reply {
        let Ok(method) = hyper::Method::from_bytes(method.as_bytes()) else {
            return crate::json_reply(
                hyper::StatusCode::BAD_REQUEST,
                &serde_json::json!({"error": {"code": "invalid_request", "message": "no such method"}}),
            );
        };
        let mut map = hyper::HeaderMap::new();
        for (name, value) in headers {
            if let (Ok(name), Ok(value)) = (
                hyper::header::HeaderName::from_bytes(name.as_bytes()),
                hyper::header::HeaderValue::from_str(value),
            ) {
                map.append(name, value);
            }
        }
        if let Ok(value) = hyper::header::HeaderValue::from_str(&self.authorization) {
            map.insert(hyper::header::AUTHORIZATION, value);
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        self.server.dispatch(&method, path, query, &map, body).await
    }

    /// [`Self::request`]'s answer, as a binding hands it to its language:
    /// its status, its headers and its body, whole or, for a stream of
    /// events, a chunk at a time.
    pub async fn answer(
        &self,
        method: &str,
        target: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Answer {
        Answer::of(self.request(method, target, headers, body).await).await
    }
}

/// An answer of the API, as the SDKs' native parts hand it on.
pub struct Answer {
    pub status: u16,
    /// Its headers, a name's values joined with `, `.
    pub headers: Vec<(String, String)>,
    pub body: AnswerBody,
}

/// An answer's body.
pub enum AnswerBody {
    /// Every byte of it.
    Whole(Bytes),
    /// A stream of server-sent events (`text/event-stream`), read as it comes.
    Stream(Chunks),
}

impl Answer {
    async fn of(reply: Reply) -> Self {
        let (parts, body) = reply.into_parts();
        let mut headers: Vec<(String, String)> = Vec::new();
        for (name, value) in &parts.headers {
            let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
            match headers.iter_mut().find(|(seen, _)| seen == name.as_str()) {
                Some((_, values)) => {
                    values.push_str(", ");
                    values.push_str(&value);
                }
                None => headers.push((name.as_str().to_owned(), value)),
            }
        }
        let streams = parts
            .headers
            .get(hyper::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let body = if streams {
            AnswerBody::Stream(Chunks(tokio::sync::Mutex::new(Some(body))))
        } else {
            let Ok(whole) = body.collect().await;
            AnswerBody::Whole(whole.to_bytes())
        };
        Self {
            status: parts.status.as_u16(),
            headers,
            body,
        }
    }
}

/// A streamed body's chunks, in order.
pub struct Chunks(tokio::sync::Mutex<Option<Body>>);

impl Chunks {
    /// The next chunk of data, or `None` once the stream has ended. Callers
    /// may ask from any task; each chunk is answered once.
    pub async fn next(&self) -> Option<Bytes> {
        next_data(&mut *self.0.lock().await).await
    }
}

/// The next chunk of data of `body`, which is `None` once it ended.
async fn next_data(body: &mut Option<Body>) -> Option<Bytes> {
    loop {
        let Some(frame) = body.as_mut()?.frame().await else {
            *body = None;
            return None;
        };
        let Ok(frame) = frame;
        if let Ok(data) = frame.into_data()
            && !data.is_empty()
        {
            return Some(data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Answer, AnswerBody};
    use http_body_util::{BodyExt as _, StreamBody};
    use hyper::body::{Bytes, Frame};

    #[tokio::test]
    async fn a_whole_answer_is_read_whole_and_its_repeated_headers_joined() {
        let reply = hyper::Response::builder()
            .status(201)
            .header("content-type", "application/json")
            .header("vary", "accept")
            .header("vary", "origin")
            .body(crate::body("{}"))
            .unwrap();
        let answer = Answer::of(reply).await;
        assert_eq!(answer.status, 201);
        assert_eq!(
            answer.headers,
            [
                ("content-type".to_owned(), "application/json".to_owned()),
                ("vary".to_owned(), "accept, origin".to_owned()),
            ]
        );
        assert!(matches!(answer.body, AnswerBody::Whole(bytes) if bytes == "{}"));
    }

    #[tokio::test]
    async fn an_event_stream_is_read_a_chunk_at_a_time_until_it_ends() {
        let chunks = ["event: run\n", "", "data: {}\n\n"]
            .map(|chunk| Ok::<_, std::convert::Infallible>(Frame::data(Bytes::from(chunk))));
        let reply = hyper::Response::builder()
            .header("content-type", "text/event-stream; charset=utf-8")
            .body(StreamBody::new(futures_util::stream::iter(chunks)).boxed())
            .unwrap();
        let AnswerBody::Stream(stream) = Answer::of(reply).await.body else {
            panic!("an event stream is answered as a stream");
        };
        assert_eq!(stream.next().await.unwrap(), "event: run\n");
        // An empty chunk is no chunk.
        assert_eq!(stream.next().await.unwrap(), "data: {}\n\n");
        assert!(stream.next().await.is_none());
        assert!(stream.next().await.is_none(), "it stays ended");
    }
}
