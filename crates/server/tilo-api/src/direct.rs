//! Tilo's API served in process, as a key its caller holds, against a Tilo
//! server the caller reaches directly: what the SDKs' native part and the
//! `tilo` command use when there is no API server between them and it.
//!
//! It is the API server itself ([`Server`]), with one key in its memory: the
//! caller's own, a device of the person's (a computer `tilo login` made one)
//! or a grant of theirs, with the certificate the person's root (or a key of
//! theirs) gave it. Every route is answered as on an API server, by the same
//! code, and the key dials the server as one of the person's devices does;
//! the server decides everything, as it decides for an API server. Nothing here
//! knows what kind of server it reaches.
//!
//! A key is carried as a key file ([`KeyFile`]): its secret and its
//! certificate, hex, and the host it reaches.
//!
//! A Tilo desktop serves the same server for its own person ([`Direct::here`]):
//! as its own device key, inside the desktop, which answers each workspace
//! itself. The person's programs on that computer reach it over a socket of
//! the login's own ([`Direct::serve`]).

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
    /// The person's Tilo on the web, when the key knows it (`https://tilo.sh`):
    /// the site whose page opens the links the host hands out. A key `tilo
    /// login` made carries the site the device logged in with, so a link it
    /// asks for is the one the site's own page would have made. Without it
    /// the host's own origin stands in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
    /// What the host's people see it as, and History names its runs.
    pub name: String,
    /// The key's 32-byte secret, hex.
    pub key: String,
    /// Its certificate, canonical bytes as hex.
    pub cert: String,
}

impl KeyFile {
    /// `key` with `cert`, reaching `host`, as a device of the person's on
    /// `site` (`None` when the key knows no site), named `name`.
    #[must_use]
    pub fn new(
        host: &str,
        site: Option<&str>,
        name: &str,
        key: &ed25519_dalek::SigningKey,
        cert: &DeviceCert,
    ) -> Self {
        Self {
            host: host.trim_end_matches('/').to_owned(),
            site: site
                .map(|site| site.trim_end_matches('/').to_owned())
                .filter(|site| !site.is_empty()),
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

/// Whether `pid` lives in this process's user and mount namespaces: what tells
/// a program of this login from one a sandbox put in a namespace of its own.
///
/// A session's other sockets admit a connection by exactly this
/// (`tilo_session::mcp_server`), so a program admitted there is admitted here
/// and a sandboxed one is not.
fn shares_security_namespaces(pid: u32) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    let same = |kind: &str| {
        let ours = std::fs::metadata(format!("/proc/self/ns/{kind}"));
        let theirs = std::fs::metadata(format!("/proc/{pid}/ns/{kind}"));
        match (ours, theirs) {
            (Ok(ours), Ok(theirs)) => ours.dev() == theirs.dev() && ours.ino() == theirs.ino(),
            // A process that cannot be read is not one this desktop knows.
            _ => false,
        }
    };
    same("user") && same("mnt")
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
        let (server, key) = Self::server_for(file)?;
        Self::as_key(server, &key, &file.name)
    }

    /// [`Self::new`], every call going over `device`, the session with the
    /// key's server its caller already holds as that key: a program holding
    /// its device's one session (`tilo mcp`) answers the API on it, since a
    /// second session of the same key would cut the first off. When that
    /// session ends, calls answer that it is unavailable.
    ///
    /// # Errors
    /// The key or its certificate is not one, or does not hold now.
    pub fn over(
        file: &KeyFile,
        device: tilo_session_client::device::Device,
    ) -> Result<Self, String> {
        let (server, key) = Self::server_for(file)?;
        let held = server
            .hosts
            .first()
            .map(|first| Arc::new(first.held(&key, device)))
            .ok_or_else(|| "the key names no server".to_owned())?;
        let _ = server.held.set(held);
        Self::as_key(server, &key, &file.name)
    }

    /// The server `file`'s key reaches, and the key.
    fn server_for(file: &KeyFile) -> Result<(Arc<Server>, GrantKey), String> {
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
        // Links the host hands over are built on the person's site, as its
        // own page builds them, and only the host it reaches is the host.
        let site = file
            .site
            .as_deref()
            .map(|site| site.trim_end_matches('/'))
            .filter(|site| !site.is_empty())
            .unwrap_or(host);
        let server = Server::new(
            &Config {
                issuer: host.to_owned(),
                web_entry: site.to_owned(),
                hosts: vec![host.to_owned()],
            },
            Store::in_memory(),
        )
        .map_err(|error| error.to_string())?;
        Ok((server, key))
    }

    /// The API of `here`, the Tilo desktop this is part of, as its device key
    /// `key` with `cert`, named `name` (History names its sessions so).
    ///
    /// # Errors
    /// The certificate is not for `key`, or does not hold now.
    pub fn here(
        here: Arc<dyn crate::session::Here>,
        key: ed25519_dalek::SigningKey,
        cert: DeviceCert,
        name: &str,
    ) -> Result<Self, String> {
        let key = GrantKey::held(key, cert, now_ms().saturating_mul(1_000_000))
            .map_err(|refused| refused.to_string())?;
        let server = Server::here(here).map_err(|error| error.to_string())?;
        Self::as_key(server, &key, name)
    }

    fn as_key(server: Arc<Server>, key: &GrantKey, name: &str) -> Result<Self, String> {
        let (_, token) = server
            .store
            .resource_key(key, name, GrantKind::Key, None, now_ms())
            .map_err(|error| error.to_string())?;
        Ok(Self {
            server,
            authorization: format!("Bearer {token}"),
        })
    }

    /// Serve the API on `listener` until it closes, every request as this
    /// key: to programs of the account that owns the socket, and nobody else.
    ///
    /// The kernel's account of the connection's peer is the whole of the
    /// admission: its account must be this session's, and it must live in this
    /// login's own user and mount namespaces, as this session's other sockets
    /// require ([`crate::direct::shares_security_namespaces`]). A program a
    /// sandbox put in a namespace of its own is the same account and is
    /// refused all the same.
    pub async fn serve(self, listener: tokio::net::UnixListener) {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(authorization) = hyper::header::HeaderValue::from_str(&self.authorization) else {
            return;
        };
        // Only this process's own account is served.
        let owner = std::fs::metadata("/proc/self")
            .ok()
            .map(|metadata| metadata.uid());
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let admitted = stream
                .peer_cred()
                .ok()
                .filter(|credentials| Some(credentials.uid()) == owner)
                .and_then(|credentials| credentials.pid())
                .and_then(|pid| u32::try_from(pid).ok())
                .is_some_and(shares_security_namespaces);
            if !admitted {
                tracing::warn!("API: a connection from another account or a sandbox is refused");
                continue;
            }
            tokio::spawn(crate::serve_connection(
                Arc::clone(&self.server),
                stream,
                Some(authorization.clone()),
            ));
        }
    }

    /// Answer one MCP request (a `tools/call`) as the API's MCP endpoint
    /// answers it, on this key's own session for the API's calls: its answer. What
    /// a person's account and a workspace's people do (`account/…`,
    /// `workspace/members#…`) is answered here as for any key of theirs.
    ///
    /// # Errors
    /// The key cannot act now, or the call was not answered.
    pub async fn mcp(&self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        let mut headers = hyper::HeaderMap::new();
        let value = hyper::header::HeaderValue::from_str(&self.authorization)
            .map_err(|_| "this key cannot act now".to_owned())?;
        headers.insert(hyper::header::AUTHORIZATION, value);
        let grant = self
            .server
            .grant_of(&headers)
            .map_err(|_| "this key cannot act now".to_owned())?;
        let session = self
            .server
            .api_session(&grant)
            .await
            .map_err(|error| error.body.message)?;
        session
            .handle(request)
            .await
            .messages
            .into_iter()
            .last()
            .ok_or_else(|| "your workspace didn't answer; try again in a minute".to_owned())
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
    use super::{Answer, AnswerBody, Direct, KeyFile};
    use http_body_util::{BodyExt as _, StreamBody};
    use hyper::body::{Bytes, Frame};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

    /// A site's shelf: an invitation left on it is kept, and anything else
    /// is not the shelf's.
    async fn shelf_site() -> String {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0u8; 8192];
                let read = stream.read(&mut request).await.unwrap_or_default();
                let kept = String::from_utf8_lossy(&request[..read]).starts_with("POST /i/");
                let status = if kept {
                    "204 No Content"
                } else {
                    "404 Not Found"
                };
                let _ = stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        origin
    }

    /// The key file a device's login makes knows the site it logged in with,
    /// so a link it asks the host for is built on that site — the one the
    /// person's page builds its invitations on — and never on the server the
    /// key reaches.
    #[tokio::test]
    async fn a_link_a_key_asks_for_is_built_on_its_site() {
        let site = shelf_site().await;
        let (person, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(30 * 24 * 3600 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).unwrap();
        // A server no test listens on: a link built on it would be one nothing
        // can open, so the site's own address is what says the site was used.
        let server = "http://127.0.0.1:9";
        let direct = Direct::new(&KeyFile::new(
            server,
            Some(&site),
            "Ada's laptop",
            &key,
            &cert,
        ))
        .unwrap();

        let workspace = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";
        let link = direct
            .server
            .invitation_link(workspace, b"signed", now.saturating_add(600_000_000_000))
            .await;
        assert!(
            link.starts_with(&format!("{site}/i/")),
            "the link is built on the site the device logged in with: {link}"
        );
        assert!(!link.contains(server), "{link}");
        assert!(
            link.contains('#'),
            "the key it opens with is in the fragment: {link}"
        );
    }

    /// A desktop holding one workspace, which answers every program with what it was asked
    /// and the name of the session it was entered for.
    struct Desktop;

    const WORKSPACE: &str = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";

    struct Echo(String);

    impl crate::session::Answering for Echo {
        fn handle(
            &self,
            request: serde_json::Value,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Option<serde_json::Value>> + Send + '_>,
        > {
            let program = request
                .pointer("/params/arguments/program")
                .cloned()
                .unwrap_or_default();
            let text = format!("{} in {}", program.as_str().unwrap_or_default(), self.0);
            Box::pin(async move {
                Some(
                    serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": {
                        "structuredContent": {"status": "ok", "output": [text.clone()], "text": text},
                    }}),
                )
            })
        }
    }

    impl crate::session::Here for Desktop {
        fn workspaces(&self) -> Vec<crate::session::Workspace> {
            vec![crate::session::held(WORKSPACE.into(), "Release".into())]
        }

        fn enter(
            &self,
            workspace: &str,
            _session: &str,
            label: &str,
        ) -> Result<Box<dyn crate::session::Answering>, String> {
            if workspace != WORKSPACE {
                return Err(format!("no workspace {workspace} here"));
            }
            Ok(Box::new(Echo(label.to_owned())))
        }
    }

    /// A program on this computer reaches the desktop's API at its socket, as the desktop's own
    /// device, with no key of its own: it lists the workspaces the desktop holds and has a
    /// program done in one, which the desktop answers. The account's own actions are not answered.
    #[tokio::test]
    async fn a_desktop_serves_its_persons_programs_at_its_socket() {
        let (person, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(3600 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).unwrap();
        let direct = Direct::here(std::sync::Arc::new(Desktop), key, cert, "Ada's laptop").unwrap();
        let dir = std::env::temp_dir().join(format!("tilo-api-here-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(direct.serve(listener));

        let client = crate::client::Client::session(&socket);
        let listed = client.get("/v1/workspaces").await.unwrap();
        assert_eq!(listed["workspaces"][0]["id"], WORKSPACE, "{listed}");
        assert_eq!(listed["workspaces"][0]["name"], "Release", "{listed}");
        let ran = client
            .post(
                &format!("/v1/workspaces/{WORKSPACE}/programs"),
                &serde_json::json!({"program": "notes.notebook.notes.len()"}),
            )
            .await
            .unwrap();
        assert_eq!(ran["status"], "ok", "{ran}");
        assert_eq!(
            ran["text"], "notes.notebook.notes.len() in Ada's laptop #1",
            "the program acts as the desktop's own device: {ran}"
        );
        assert!(
            !ran["session"].as_str().unwrap_or_default().is_empty(),
            "{ran}"
        );
        let refused = client
            .post("/v1/workspaces", &serde_json::json!({"name": "Another"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("answers its own workspaces only"),
            "{refused}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A request with no `Authorization` at all is answered as the desktop's own key: a program of
    /// this login sends no secret, because the kernel's account of its peer admits the connection
    /// and the desktop acts as the one key it holds.
    #[tokio::test]
    async fn a_request_with_no_bearer_is_answered_as_the_persons_key() {
        use http_body_util::BodyExt as _;
        use hyper_util::rt::TokioIo;
        let (person, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
        let now = tilo_clock::unix_now_ns();
        let mut cert = DeviceCert {
            user_public_key: person.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(3600 * 1_000_000_000)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &person).unwrap();
        let direct = Direct::here(std::sync::Arc::new(Desktop), key, cert, "Ada's laptop").unwrap();
        let dir = std::env::temp_dir().join(format!("tilo-api-here-bare-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(direct.serve(listener));

        // No `Authorization` header, as `Client::session` sends none.
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(connection);
        let request = hyper::Request::builder()
            .method("GET")
            .uri("/v1/workspaces")
            .body(http_body_util::Full::new(hyper::body::Bytes::new()))
            .unwrap();
        let answer = sender.send_request(request).await.unwrap();
        assert_eq!(answer.status(), 200, "a request with no key is answered");
        let body = answer.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            listed["workspaces"][0]["id"], WORKSPACE,
            "the desktop answers as its own key: {listed}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
