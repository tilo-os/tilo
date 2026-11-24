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
//! the login's own ([`Direct::serve`]). A computer `tilo login` made a device
//! of answers in its daemon, which keeps that device's one session, so no
//! program of its opens a second one.

use crate::grant_key::GrantKey;
use crate::store::{GrantKind, Store};
use crate::{Body, Config, Reply, Server, now_ms};
use http_body_util::BodyExt as _;
use hyper::body::Bytes;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tilo_user_identity::DeviceCert;

/// Whether `key_file`, or the login `token` carries, names this computer's
/// own device key: only then do a program's calls go to the session the
/// computer holds. `None` for both is the login itself, and so this
/// computer's.
///
/// Only the key's public half is compared, with a certificate of this
/// computer's device key, so naming the device's own key keeps this process
/// dumpable, which the daemon requires of a caller it admits.
pub(crate) fn names_this_device(
    key_file: Option<&std::path::Path>,
    token: Option<String>,
) -> anyhow::Result<bool> {
    let named: Option<[u8; 32]> = match (key_file, token) {
        (Some(path), _) => {
            let file = KeyFile::read(path).map_err(anyhow::Error::msg)?;
            public_key_of(&file.key)
        }
        (None, Some(token)) => {
            crate::store::carried_key(&token).map(|(_, key)| key.key().verifying_key().to_bytes())
        }
        (None, None) => return Ok(true),
    };
    // A key this computer cannot read is no device key.
    let Some(named) = named else {
        return Ok(false);
    };
    let device = tilo_device_identity::login::served_device_key(&crate::calls::device_keys());
    Ok(device == Some(named))
}

/// The public key the 32-byte hex secret in a key file names.
fn public_key_of(secret: &str) -> Option<[u8; 32]> {
    let seed: [u8; 32] = hex::decode(secret).ok()?.try_into().ok()?;
    Some(
        ed25519_dalek::SigningKey::from_bytes(&seed)
            .verifying_key()
            .to_bytes(),
    )
}

/// A key as a file holds it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyFile {
    /// The Tilo server it reaches: an origin (`https://eu1.tilo.sh`).
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
    /// The server's certificate and iroh address, when the key reaches it over iroh only: a server
    /// behind a NAT, reached through a packet relay, has no web origin to ask them of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reached_at: Option<crate::server::ServerInfo>,
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
            reached_at: None,
        }
    }

    /// This file, reaching its host as `info` says (`web-host.json`): where a login
    /// this computer kept names it, and what a call dials instead of asking its web origin.
    #[must_use]
    pub fn reaching_at(mut self, info: crate::server::ServerInfo) -> Self {
        self.reached_at = Some(info);
        self
    }

    /// The key `token` carries (`tilo_sk_…`, as an approved key is answered),
    /// reaching where this file's key reaches.
    ///
    /// # Errors
    /// The token carries no key.
    pub fn with_token(self, token: &str) -> Result<Self, String> {
        let (name, key) =
            crate::store::carried_key(token).ok_or_else(|| "the answer is not a key".to_owned())?;
        Ok(Self {
            name,
            key: hex::encode(key.secret()),
            cert: hex::encode(key.cert_bytes()),
            ..self
        })
    }

    /// The key file at `path`: a key file, or a key alone as an action answers one (what
    /// `tilo do account/keys#create --out FILE` writes, and the SDKs take), which reaches the
    /// server this computer's login reaches.
    ///
    /// # Errors
    /// It cannot be read, or is neither, or holds a key alone on a computer not logged in.
    pub fn read(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if let Ok(file) = serde_json::from_slice(&bytes) {
            return Ok(file);
        }
        let token = String::from_utf8_lossy(&bytes);
        crate::calls::script_key(token.trim())
            .map_err(|error| format!("{}: {error:#}", path.display()))?
            .ok_or_else(|| {
                format!(
                    "{}: a key alone reaches the server this computer's login reaches, and this \
                     computer is not logged in (tilo login)",
                    path.display()
                )
            })
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
#[derive(Clone)]
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
                // A login this computer kept names where its host is dialled; one it did not
                // dials nothing, and a call that needs a host says so.
                hosts: file.reached_at.iter().cloned().collect(),
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

    /// Serve the API on `listener` until it closes, every request as this key: to programs of
    /// the account that owns the socket, and nobody else.
    ///
    /// The kernel's account of the connection's peer is the whole of the admission: its account
    /// must be this process's, and it must live in this login's own user and mount namespaces,
    /// as a session's other sockets require ([`shares_security_namespaces`]). A program a sandbox
    /// put in a namespace of its own is the same account and is refused all the same.
    pub async fn serve(self, listener: tilo_files::native::socket::tokio::Listener) {
        use std::os::unix::fs::MetadataExt as _;
        let this = Arc::new(self);
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
            let this = Arc::clone(&this);
            tokio::spawn(async move { this.serve_one(stream).await });
        }
    }

    /// Serve one admitted connection as this key.
    async fn serve_one(&self, stream: tilo_files::native::socket::tokio::Stream) {
        let Ok(authorization) = hyper::header::HeaderValue::from_str(&self.authorization) else {
            return;
        };
        // The command on the other end of this connection waits for this: one that takes
        // seconds is named in a stall report rather than left as an unexplained wait.
        let _doing = tilo_rt::stall::doing("a command through this computer's session");
        crate::serve_connection(Arc::clone(&self.server), stream, Some(authorization)).await;
    }

    /// Answer one MCP request (a `tools/call`) as the API's MCP endpoint
    /// answers it, on this key's own session for the API's calls: its answer. What a
    /// person's account does (`account/…`) is answered here as for any key of theirs.
    ///
    /// # Errors
    /// The key cannot act now, or the call was not answered.
    pub async fn mcp(&self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        self.session()
            .await?
            .handle(request)
            .await
            .messages
            .into_iter()
            .last()
            .ok_or_else(|| "your workspace didn't answer; try again in a minute".to_owned())
    }

    /// This key's one session, opened the first time.
    async fn session(&self) -> Result<Arc<crate::session::Session>, String> {
        let mut headers = hyper::HeaderMap::new();
        let value = hyper::header::HeaderValue::from_str(&self.authorization)
            .map_err(|_| "this key cannot act now".to_owned())?;
        headers.insert(hyper::header::AUTHORIZATION, value);
        let grant = self
            .server
            .grant_of(&headers)
            .map_err(|_| "this key cannot act now".to_owned())?;
        self.server
            .api_session(&grant)
            .await
            .map_err(|error| error.body.message)
    }

    /// Call `tool` with `arguments` in `workspace` (`None`: the account's own tree) on this
    /// key's session, as a step of `group` when one is named: its typed answer.
    ///
    /// # Errors
    /// The call was refused.
    pub async fn typed(
        &self,
        workspace: Option<&str>,
        tool: &str,
        arguments: serde_json::Value,
        group: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, crate::v1::errors::ApiError> {
        self.session()
            .await
            .map_err(|why| crate::v1::errors::ApiError::new("unavailable", why))?
            .typed(workspace, tool, arguments, group)
            .await
    }

    /// Run the program `arguments` names (`{program, confirm}`) in `workspace` on this key's
    /// session, in the program session `session` names (its `let`s, the references its answers
    /// showed), or a new one: its answer, and the program session to continue.
    ///
    /// # Errors
    /// The program was refused, or that program session ended.
    pub async fn program(
        &self,
        workspace: &str,
        session: Option<&str>,
        arguments: serde_json::Value,
        group: Option<serde_json::Value>,
    ) -> Result<(serde_json::Value, String), crate::v1::errors::ApiError> {
        self.session()
            .await
            .map_err(|why| crate::v1::errors::ApiError::new("unavailable", why))?
            .run_program(workspace, session, arguments, group)
            .await
    }

    /// `actions`, the actions of `workspace`, with this key's account beside them: the tree a
    /// program of the person's walks, where `account/…` is answered as for any key of theirs.
    ///
    /// # Errors
    /// The key cannot act now.
    pub async fn joined(
        &self,
        workspace: &str,
        actions: crate::session::Provider,
    ) -> Result<crate::session::Provider, String> {
        let account = self.session().await?.account().clone();
        Ok(crate::account::Joined::provider(
            workspace,
            Some(actions),
            account,
        ))
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
    use super::{Answer, AnswerBody, Direct};
    use http_body_util::{BodyExt as _, StreamBody};
    use hyper::body::{Bytes, Frame};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert, UserIdentity};

    /// A desktop holding one workspace, whose actions are the app it publishes, and the
    /// workspaces it was entered for.
    #[derive(Default)]
    struct Desktop {
        actions: tilo_mcp::test_hosts::StubHost,
        entered: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    const WORKSPACE: &str = "3ee4a797-1ec0-474b-a3d0-62895d5e15af";

    /// A device key seeded `seed` and the certificate Ada's root signed for it, valid for an hour.
    fn ada_laptop(seed: u8) -> (ed25519_dalek::SigningKey, DeviceCert) {
        let (person, _) = UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[seed; 32]);
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
        (key, cert)
    }

    impl crate::session::Here for Desktop {
        fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            vec![crate::session::held(WORKSPACE.into(), "Release".into())]
        }

        fn enter(
            &self,
            workspace: &str,
            _: &crate::grant_key::GrantKey,
        ) -> Result<crate::session::Reached, String> {
            if workspace != WORKSPACE {
                return Err(format!("no workspace {workspace} here"));
            }
            self.entered.lock().unwrap().push(workspace.to_owned());
            let provider: crate::session::Provider = std::sync::Arc::new(self.actions.clone());
            Ok(crate::session::Reached {
                provider,
                person: "ada".into(),
                caller: None,
                tasks: None,
            })
        }
    }

    /// A program on this computer reaches the desktop's own API socket as the desktop's own
    /// device, with no key of its own, one MCP tool call a call: it lists the workspaces the
    /// desktop is in and lists what one has, which the desktop's own actions answer. The
    /// account's own actions are not answered.
    #[tokio::test]
    async fn a_desktop_serves_its_persons_programs_at_its_socket() {
        let (key, cert) = ada_laptop(7);
        let desktop = Desktop::default();
        let entered = std::sync::Arc::clone(&desktop.entered);
        let direct = Direct::here(std::sync::Arc::new(desktop), key, cert, "Ada's laptop").unwrap();
        let dir = std::env::temp_dir().join(format!("tilo-api-here-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        tokio::spawn(direct.serve(listener));

        let calls = crate::calls::Calls::desktop(socket);
        let listed = calls.workspaces().await.unwrap();
        assert_eq!(listed["workspaces"][0]["id"], WORKSPACE, "{listed}");
        assert_eq!(listed["workspaces"][0]["name"], "Release", "{listed}");
        let apps = calls.list(Some(WORKSPACE), None, None).await.unwrap();
        assert!(apps["rows"].is_array(), "{apps}");
        assert_eq!(
            *entered.lock().unwrap(),
            [WORKSPACE],
            "the desktop's own actions answer the listing: {apps}"
        );
        let refused = calls
            .account(
                "account/workspaces",
                "create",
                &serde_json::json!({"name": "Another"}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(
            refused.contains("answers its own workspaces only"),
            "{refused}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A device of the person's that keeps the workspaces it was asked to, and reaches their
    /// account at a server that lists one more, shared with them.
    #[derive(Default)]
    struct Device {
        kept: std::sync::Mutex<Vec<(String, String)>>,
    }

    const SHARED: &str = "0a6c1f5e-9d84-4b0e-8f2d-4c3a1b2e7d90";
    const MADE: &str = "5b1d7c3e-2a4f-4e6b-9c8d-7f0e1a2b3c4d";

    /// The person's account at their server: it makes the workspaces it is asked to, owned by
    /// the person, and lists them beside [`SHARED`], which another person shared with them.
    #[derive(Default)]
    struct HomeServer {
        made: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl crate::session::AccountServer for HomeServer {
        fn call(
            &self,
            target: tilo_actions::remote::RemoteActionTarget,
            arguments: serde_json::Map<String, serde_json::Value>,
        ) -> crate::session::Asked<'_, serde_json::Value> {
            use tilo_server_protocol::workspaces as ws;
            let text = |name: &str| arguments[name].as_str().unwrap().to_owned();
            let action = target.address.action.as_str();
            let answer = match action {
                ws::CREATE_OWN_WORKSPACE => {
                    let (id, name) = (text("workspace"), text("name"));
                    self.made.lock().unwrap().push((id.clone(), name.clone()));
                    serde_json::to_value(ws::CreatedWorkspace {
                        workspace: ws::WorkspaceListing {
                            workspace_id: tilo_server_protocol::WorkspaceId::parse(&id).unwrap(),
                            workspace: id,
                            name,
                            generation: 1,
                            role: Some("owner".to_owned()),
                            labels: ws::WorkspaceLabels::new(),
                            unattended: false,
                            made_at_ms: None,
                        },
                        invitation: None,
                    })
                    .map_err(|error| error.to_string())
                }
                ws::LIST_PERSONAL => {
                    let row = |id: &str, name: &str, role: &str| ws::PersonalWorkspace {
                        workspace: id.to_owned(),
                        name: name.to_owned(),
                        host: None,
                        role: Some(role.to_owned()),
                        labels: ws::WorkspaceLabels::new(),
                        made_at_ms: None,
                        unattended: false,
                        enter_at: None,
                    };
                    let mut workspaces = vec![row(SHARED, "Shared", "editor")];
                    workspaces.extend(
                        self.made
                            .lock()
                            .unwrap()
                            .iter()
                            .map(|(id, name)| row(id, name, "owner")),
                    );
                    serde_json::to_value(ws::PersonalList { workspaces })
                        .map_err(|error| error.to_string())
                }
                _ => Err(format!("{action} is not this server's")),
            };
            Box::pin(async move { answer })
        }
    }

    impl crate::session::Here for Device {
        fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            self.kept
                .lock()
                .unwrap()
                .iter()
                .map(|(id, name)| crate::session::held(id.clone(), name.clone()))
                .collect()
        }

        fn enter(
            &self,
            workspace: &str,
            _: &crate::grant_key::GrantKey,
        ) -> Result<crate::session::Reached, String> {
            Err(format!("{workspace} is not entered in this test"))
        }

        fn account(&self) -> Option<std::sync::Arc<dyn crate::session::AccountServer>> {
            static HOME: std::sync::OnceLock<std::sync::Arc<HomeServer>> =
                std::sync::OnceLock::new();
            Some(HOME.get_or_init(std::sync::Arc::default).clone())
        }

        fn keep(&self, workspace: &str) -> crate::session::Asked<'_, ()> {
            let kept = (workspace.to_owned(), String::new());
            self.kept.lock().unwrap().push(kept);
            Box::pin(async { Ok(()) })
        }

        fn leave(&self, workspace: &str) -> crate::session::Asked<'_, ()> {
            self.kept.lock().unwrap().retain(|(id, _)| id != workspace);
            Box::pin(async { Ok(()) })
        }
    }

    /// A device with no login answers the person's account itself: a workspace it makes at
    /// their server is kept here, its list says which of theirs it keeps, and it enters and
    /// leaves one shared with them at their word.
    #[tokio::test]
    async fn a_device_makes_lists_enters_and_leaves_workspaces_from_its_own_state() {
        let (key, cert) = ada_laptop(13);
        let direct = Direct::here(
            std::sync::Arc::new(Device::default()),
            key,
            cert,
            "Ada's laptop",
        )
        .unwrap();
        let ask = |action: &'static str, body: serde_json::Value| {
            let target = format!("/v1/resources/account/workspaces/actions/{action}");
            let direct = &direct;
            async move {
                let headers = [("content-type".to_owned(), "application/json".to_owned())];
                let answer = direct
                    .answer("POST", &target, &headers, body.to_string().as_bytes())
                    .await;
                let AnswerBody::Whole(bytes) = answer.body else {
                    panic!("a whole answer");
                };
                let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(answer.status, 200, "{action}: {json}");
                json["result"].clone()
            }
        };
        let kept = |listed: &serde_json::Value| -> Vec<(String, bool, bool)> {
            listed["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    (
                        row["id"].as_str().unwrap().to_owned(),
                        row["own"].as_bool().unwrap(),
                        row["kept_here"].as_bool().unwrap(),
                    )
                })
                .collect()
        };

        let made = ask(
            "create",
            serde_json::json!({"name": "Made here", "id": MADE}),
        )
        .await;
        assert_eq!(made["workspace"]["id"], MADE, "{made}");
        assert_eq!(
            made["workspace"]["kept_here"], true,
            "made here is kept here: {made}"
        );

        let listed = ask("list", serde_json::json!({})).await;
        assert_eq!(
            kept(&listed),
            [
                (MADE.to_owned(), true, true),
                (SHARED.to_owned(), false, false)
            ],
            "{listed}"
        );

        let entered = ask("enter", serde_json::json!({"workspace": SHARED})).await;
        assert_eq!(entered["kept_here"], true, "{entered}");
        let listed = ask("list", serde_json::json!({})).await;
        assert!(
            kept(&listed).contains(&(SHARED.to_owned(), false, true)),
            "{listed}"
        );

        let left = ask("leave", serde_json::json!({"workspace": SHARED})).await;
        assert_eq!(left["kept_here"], false, "{left}");
        let listed = ask("list", serde_json::json!({})).await;
        assert!(
            kept(&listed).contains(&(SHARED.to_owned(), false, false)),
            "the person stays in what this device left: {listed}"
        );
    }

    /// A request with no `Authorization` at all is answered as the desktop's own key: a program of
    /// this login sends no secret, because the kernel's account of its peer admits the connection
    /// and the desktop acts as the one key it holds.
    #[tokio::test]
    async fn a_request_with_no_bearer_is_answered_as_the_persons_key() {
        use http_body_util::BodyExt as _;
        use hyper_util::rt::TokioIo;
        let (key, cert) = ada_laptop(11);
        let direct = Direct::here(
            std::sync::Arc::new(Desktop::default()),
            key,
            cert,
            "Ada's laptop",
        )
        .unwrap();
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
            .method("POST")
            .uri("/v1/resources/account/workspaces/actions/list")
            .header("content-type", "application/json")
            .body(http_body_util::Full::new(hyper::body::Bytes::from_static(
                b"{}",
            )))
            .unwrap();
        let answer = sender.send_request(request).await.unwrap();
        assert_eq!(answer.status(), 200, "a request with no key is answered");
        let body = answer.into_body().collect().await.unwrap().to_bytes();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            listed["result"]["workspaces"][0]["id"], WORKSPACE,
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
