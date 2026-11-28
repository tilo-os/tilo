//! Tilo's API served in process, as a key its caller holds, against a Tilo
//! server the caller reaches directly: what the `tilo` command uses with a key
//! file when there is no API server between it and the server.
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
//! the login's own, each one the person said yes to ([`Direct::serve_admitted`]).
//! A computer `tilo login` made a device of answers in its daemon, which keeps
//! that device's one session, so no program of its opens a second one.

use crate::grant_key::GrantKey;
use crate::store::{GrantKind, Store};
use crate::{Config, Reply, Server, now_ms};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tilo_user_identity::DeviceCert;

/// How long an in-process API call waits for the server's answer: long enough
/// for a call held until its action ends, short enough that a server that
/// never answers is told of as what was asked and how long it waited.
const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

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
    /// `tilo do account/keys#create --out FILE` writes), which reaches the
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

    /// Serve one connection as this key, one whoever took it already admitted: a program of the
    /// person's that the desktop took at their login's API socket, which the person said yes to,
    /// and handed to the device's daemon. Being a local program admits nothing by itself.
    pub async fn serve_admitted(&self, stream: tilo_files::native::socket::tokio::Stream) {
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
    /// The key cannot act now, or the call was not answered: a call nothing
    /// answers is named and given up after [`ANSWER_WAIT`], never waited on
    /// forever.
    pub async fn mcp(&self, request: serde_json::Value) -> Result<serde_json::Value, String> {
        let asked = request
            .pointer("/params/name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("a tool")
            .to_owned();
        let answered = tokio::time::timeout(ANSWER_WAIT, async {
            self.session()
                .await?
                .handle(request)
                .await
                .messages
                .into_iter()
                .last()
                .ok_or_else(|| "your workspace didn't answer; try again in a minute".to_owned())
        })
        .await;
        answered.unwrap_or_else(|_| {
            Err(format!(
                "the daemon did not answer {asked} within {} s: it is busy or stalled",
                ANSWER_WAIT.as_secs()
            ))
        })
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

    /// `account`, the person's own list alone: the tree a connection walks while the device holds
    /// no workspace of theirs yet, answered from what it knows (the login entry and the copies
    /// held so far), with nothing of a workspace and no admission into one.
    ///
    /// # Errors
    /// The key cannot act now.
    pub async fn account_tree(&self) -> Result<crate::session::Provider, String> {
        let account = self.session().await?.account().clone();
        Ok(crate::account::Joined::provider(
            crate::account::ROOT_TREE,
            None,
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
            return crate::v1::refusal(&crate::v1::errors::ApiError::invalid("no such method"));
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
        match tokio::time::timeout(
            ANSWER_WAIT,
            self.server.dispatch(&method, path, query, &map, body),
        )
        .await
        {
            Ok(answer) => answer,
            Err(_) => crate::v1::refusal(&crate::v1::errors::ApiError::new(
                "unanswered",
                format!(
                    "the daemon did not answer {method} {target} within {} s: it is busy or stalled",
                    ANSWER_WAIT.as_secs()
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Direct;
    use http_body_util::BodyExt as _;
    use tilo_user_identity::{DeviceCert, UserIdentity};

    use tilo_peer_state::fixture::member_cert_with;
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
        let cert = member_cert_with(
            &person,
            &key,
            "Ada's laptop",
            now.saturating_sub(60_000_000_000),
            Some(now.saturating_add(3600 * 1_000_000_000)),
        );
        (key, cert)
    }

    impl crate::session::Here for Desktop {
        fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            vec![crate::session::held(
                WORKSPACE.into(),
                "Release".into(),
                Some("owner"),
            )]
        }

        fn enter(
            &self,
            workspace: &str,
            _: &crate::grant_key::GrantKey,
        ) -> crate::session::Asked<'_, crate::session::Reached> {
            if workspace != WORKSPACE {
                let refused = format!("no workspace {workspace} here");
                return Box::pin(async move { Err(refused) });
            }
            self.entered.lock().unwrap().push(workspace.to_owned());
            let provider: crate::session::Provider = std::sync::Arc::new(self.actions.clone());
            Box::pin(async move {
                Ok(crate::session::Reached {
                    provider,
                    person: "ada".into(),
                    caller: None,
                    tasks: None,
                })
            })
        }
    }

    /// `Direct::here` serving connections to the desktop's own API socket at a fresh directory: the socket, and
    /// the directory to remove.
    fn served_here(direct: Direct, name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("tilo-api-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("api.sock");
        let _ = std::fs::remove_file(&socket);
        let listener = tilo_files::native::socket::tokio::listener(
            tilo_files::native::socket::bind(&socket).unwrap(),
        )
        .unwrap();
        // The test admits what connects, as the desktop admits a program the person said yes to.
        let direct = std::sync::Arc::new(direct);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let direct = std::sync::Arc::clone(&direct);
                tokio::spawn(async move { direct.serve_admitted(stream).await });
            }
        });
        (socket, dir)
    }

    /// One request to the API at `socket`, with no `Authorization`, as a program of the login
    /// sends it: its status and its JSON body.
    async fn ask(
        socket: &std::path::Path,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        use hyper_util::rt::TokioIo;
        let stream = tilo_files::native::socket::tokio::connect(socket)
            .await
            .unwrap();
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .unwrap();
        tokio::spawn(connection);
        let mut request = hyper::Request::builder().method(method).uri(uri);
        if body.is_some() {
            request = request.header("content-type", "application/json");
        }
        let request = request
            .body(http_body_util::Full::new(hyper::body::Bytes::from(
                body.map(|body| body.to_string()).unwrap_or_default(),
            )))
            .unwrap();
        let answer = sender.send_request(request).await.unwrap();
        let status = answer.status().as_u16();
        let body = answer.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&body).unwrap_or_default())
    }

    /// A program on this computer reaches the desktop's own API socket as the desktop's own
    /// device, with no key of its own: `/v1` lists the workspaces the desktop is in and lists
    /// what one has, which the desktop's own actions answer. A workspace is made by none of the
    /// person's devices here, and the refusal says so in words.
    #[tokio::test]
    async fn a_desktop_serves_its_persons_programs_at_its_socket() {
        let (key, cert) = ada_laptop(7);
        let desktop = Desktop::default();
        let entered = std::sync::Arc::clone(&desktop.entered);
        let direct = Direct::here(std::sync::Arc::new(desktop), key, cert, "Ada's laptop").unwrap();
        let (socket, dir) = served_here(direct, "here");

        let (_, listed) = ask(
            &socket,
            "POST",
            "/v1/resources/account/workspaces/actions/list",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(
            listed["result"]["workspaces"][0]["id"], WORKSPACE,
            "{listed}"
        );
        assert_eq!(
            listed["result"]["workspaces"][0]["name"], "Release",
            "{listed}"
        );
        let (status, apps) = ask(
            &socket,
            "GET",
            &format!("/v1/workspaces/{WORKSPACE}/resources"),
            None,
        )
        .await;
        assert_eq!(status, 200, "{apps}");
        assert!(apps["rows"].is_array(), "{apps}");
        assert_eq!(
            *entered.lock().unwrap(),
            [WORKSPACE],
            "the desktop's own actions answer the listing: {apps}"
        );
        let (_, refused) = ask(
            &socket,
            "POST",
            "/v1/resources/account/workspaces/actions/create",
            Some(serde_json::json!({"name": "Another"})),
        )
        .await;
        assert!(
            refused
                .to_string()
                .contains("none of your devices makes your workspaces"),
            "a desktop that reaches no device founding the person's workspaces says so: {refused}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A device of the person's that keeps the workspaces it was asked to, and reaches their
    /// account at a server that lists one more, shared with them.
    #[derive(Default)]
    struct Device {
        kept: std::sync::Mutex<Vec<(String, String)>>,
        /// What the person's list shows here that it has not taken in, and why.
        missing: std::sync::Mutex<Vec<(String, String)>>,
    }

    const SHARED: &str = "0a6c1f5e-9d84-4b0e-8f2d-4c3a1b2e7d90";

    /// The person's account at their server: it lists [`SHARED`], which another person shared
    /// with them, and keeps every action it is asked, which it answers none of.
    #[derive(Default)]
    struct HomeServer {
        asked: std::sync::Mutex<Vec<String>>,
    }

    impl crate::session::AccountServer for HomeServer {
        fn call(
            &self,
            target: tilo_actions::remote::RemoteActionTarget,
            _arguments: serde_json::Map<String, serde_json::Value>,
        ) -> crate::session::Asked<'_, serde_json::Value> {
            let action = target.address.action;
            self.asked.lock().unwrap().push(action.clone());
            Box::pin(async move { Err(format!("{action} is not this server's")) })
        }

        fn listed(
            &self,
            _person: &[u8; 32],
            _labels: &tilo_server_protocol::workspaces::WorkspaceLabels,
            workspace: Option<&str>,
        ) -> crate::session::Asked<'_, tilo_server_protocol::workspaces::PersonWorkspaces> {
            use tilo_server_protocol::workspaces as ws;
            let mut workspaces = vec![ws::ListedWorkspace {
                workspace: SHARED.to_owned(),
                name: "Shared".to_owned(),
                host: None,
                role: Some("editor".to_owned()),
                labels: ws::WorkspaceLabels::new(),
                made_at_ms: None,
                unattended: false,
                host_device: None,
                host_hint: None,
            }];
            workspaces.retain(|row| workspace.is_none_or(|only| row.workspace == only));
            Box::pin(async move { Ok(ws::PersonWorkspaces { workspaces }) })
        }
    }

    /// The account [`Device`] reaches, shared by every test of it.
    fn home() -> std::sync::Arc<HomeServer> {
        static HOME: std::sync::OnceLock<std::sync::Arc<HomeServer>> = std::sync::OnceLock::new();
        HOME.get_or_init(std::sync::Arc::default).clone()
    }

    impl crate::session::Here for Device {
        fn workspaces(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            self.kept
                .lock()
                .unwrap()
                .iter()
                .map(|(id, name)| crate::session::held(id.clone(), name.clone(), Some("owner")))
                .collect()
        }

        fn not_here(&self, _: &tilo_user_identity::DeviceCert) -> Vec<crate::session::Workspace> {
            self.missing
                .lock()
                .unwrap()
                .iter()
                .map(|(id, why)| crate::session::Workspace {
                    kept_here: false,
                    not_here: Some(why.clone()),
                    ..crate::session::held(id.clone(), String::new(), None)
                })
                .collect()
        }

        fn enter(
            &self,
            workspace: &str,
            _: &crate::grant_key::GrantKey,
        ) -> crate::session::Asked<'_, crate::session::Reached> {
            let refused = format!("{workspace} is not entered in this test");
            Box::pin(async move { Err(refused) })
        }

        fn account(&self) -> crate::session::FoundAccount<'_> {
            Box::pin(async {
                let home: std::sync::Arc<dyn crate::session::AccountServer> = home();
                Some(home)
            })
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

    /// A device with no login that founds no workspace itself answers the person's account
    /// itself: with no device of theirs that founds their workspaces it makes none, and never
    /// asks their server to, its list says which of theirs it keeps and why one it does not is
    /// not here yet, and it enters and leaves one shared with them at their word.
    #[tokio::test]
    async fn a_device_that_founds_nothing_lists_enters_and_leaves_workspaces_from_its_own_state() {
        let (key, cert) = ada_laptop(13);
        let device = std::sync::Arc::new(Device::default());
        let direct = Direct::here(
            std::sync::Arc::clone(&device) as std::sync::Arc<dyn crate::session::Here>,
            key,
            cert,
            "Ada's laptop",
        )
        .unwrap();
        let answered = |action: &'static str, body: serde_json::Value| {
            let target = format!("/v1/resources/account/workspaces/actions/{action}");
            let direct = &direct;
            async move {
                let headers = [("content-type".to_owned(), "application/json".to_owned())];
                let answer = direct
                    .request("POST", &target, &headers, body.to_string().as_bytes())
                    .await;
                let status = answer.status();
                let bytes = answer.into_body().collect().await.unwrap().to_bytes();
                let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                (status, json)
            }
        };
        let ask = |action: &'static str, body: serde_json::Value| async move {
            let (status, json) = answered(action, body).await;
            assert_eq!(status, 200, "{action}: {json}");
            json["result"].clone()
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

        let (status, refused) = answered(
            "create",
            serde_json::json!({"name": "Made here", "id": "5b1d7c3e-2a4f-4e6b-9c8d-7f0e1a2b3c4d"}),
        )
        .await;
        assert_ne!(status, 200, "{refused}");
        assert!(
            refused
                .to_string()
                .contains(crate::account::NO_FOUNDING_DEVICE),
            "the person is told none of their devices makes their workspaces: {refused}"
        );
        assert!(
            home().asked.lock().unwrap().is_empty(),
            "a device that founds nothing asks no server to found a workspace for it"
        );

        let listed = ask("list", serde_json::json!({})).await;
        assert_eq!(
            kept(&listed),
            [(SHARED.to_owned(), false, false)],
            "{listed}"
        );
        assert!(
            listed["workspaces"][0].get("not_here").is_none(),
            "{listed}"
        );

        // One the person's list shows here that the device could not take in says why, on the row
        // of the list it is in, and nowhere once the device keeps it.
        let away = "the device keeping it did not answer within 30 seconds";
        let made_elsewhere = "7c2e9a41-5b3d-4f8e-a1c6-0d9e8f7a6b5c";
        device.missing.lock().unwrap().extend([
            (SHARED.to_owned(), away.to_owned()),
            (made_elsewhere.to_owned(), away.to_owned()),
        ]);
        let listed = ask("list", serde_json::json!({})).await;
        let rows = listed["workspaces"].as_array().unwrap();
        assert_eq!(rows.len(), 2, "{listed}");
        assert!(rows.iter().all(|row| row["not_here"] == away), "{listed}");

        let entered = ask("enter", serde_json::json!({"workspace": SHARED})).await;
        assert_eq!(entered["kept_here"], true, "{entered}");
        let listed = ask("list", serde_json::json!({})).await;
        assert!(
            listed["workspaces"][0].get("not_here").is_none(),
            "one kept here says nothing is missing: {listed}"
        );
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
        let (key, cert) = ada_laptop(11);
        let direct = Direct::here(
            std::sync::Arc::new(Desktop::default()),
            key,
            cert,
            "Ada's laptop",
        )
        .unwrap();
        let (socket, dir) = served_here(direct, "here-bare");
        // No `Authorization` header, as `Client::session` sends none.
        let (status, listed) = ask(
            &socket,
            "POST",
            "/v1/resources/account/workspaces/actions/list",
            Some(serde_json::json!({})),
        )
        .await;
        assert_eq!(status, 200, "a request with no key is answered");
        assert_eq!(
            listed["result"]["workspaces"][0]["id"], WORKSPACE,
            "the desktop answers as its own key: {listed}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
