//! Tilo's API, `/v1`, as `tilo`'s hosted commands reach it: one client, two
//! transports, and nothing that knows which kind of host answers.
//!
//! `docs/developers/api.md` is the callers' reference. A call goes to an API
//! server over HTTP with a key (`TILO_API_KEY`, and the server named by
//! `--api` or `TILO_API_URL`), or to a Tilo host in process
//! ([`tilo_api::direct::Direct`]) as a key this computer holds: the key file
//! `--key-file` names, or, without an API key, the one `tilo login` kept here.
//! Every route answers the same either way, so no command here can tell.
//!
//! A mutating call carries an `Idempotency-Key` of its own, and every retry of
//! it sends that same key: the host never runs one invocation twice.

use anyhow::{Context as _, Result, anyhow, bail};
use http_body_util::BodyExt as _;
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

/// The API server hosted Tilo answers at.
pub const DEFAULT_API_URL: &str = "https://mcp.tilo.sh";
/// Names the API server, like `--api`.
pub const API_URL_ENV: &str = "TILO_API_URL";
/// The key a call is made with.
pub const KEY_ENV: &str = "TILO_API_KEY";
/// The most one answer may be.
const MAX_ANSWER: usize = 64 * 1024 * 1024;
/// The longest one request is held before the server answers that it still
/// waits: `Prefer: wait=` takes seconds, 60 at most.
const WAIT_SECONDS: &str = "60";
/// How long a call keeps asking while the host is busy with the person's
/// changes.
const BUSY_DEADLINE: Duration = Duration::from_secs(60);
/// How long an action waits for the person to answer it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// How long a request is given, uploads included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a call waits before asking about a pending run again.
const ANSWER_PAUSE: Duration = Duration::from_secs(1);

/// A request's method, as the routes of the API take them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Delete,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Delete => "DELETE",
        }
    }

    /// Whether it changes something, so it takes an `Idempotency-Key`.
    const fn mutating(self) -> bool {
        !matches!(self, Self::Get)
    }
}

/// What a hosted command reaches Tilo with, as the person named it.
#[derive(Clone, Copy)]
pub struct Reach<'a> {
    /// The API server, like `--api`.
    pub api: Option<&'a str>,
    /// A key file this computer holds, like `--key-file`.
    pub key_file: Option<&'a Path>,
    /// Whether the answer is JSON, typed as the API gives it.
    pub json: bool,
}

/// One answer of the API: its status and its body.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// Whether the call ran.
    fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// The refusal this answer carries, with what a caller can do instead.
    fn refused(&self) -> anyhow::Error {
        let json = self.json();
        let error = json.get("error");
        let field = |name: &str| error.and_then(|error| error.get(name));
        let message = field("message")
            .and_then(Value::as_str)
            .unwrap_or("the API refused the call");
        let mut said = field("code").and_then(Value::as_str).map_or_else(
            || format!("{message} (HTTP {})", self.status),
            |code| format!("{message} ({code})"),
        );
        for step in field("next_steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(step) = step.as_str() {
                said.push_str("\n  ");
                said.push_str(step);
            }
        }
        anyhow!("{said}")
    }

    /// How long the answer asks the caller to wait before the same call works,
    /// when it says the host is busy with the person's changes.
    fn busy_for(&self) -> Option<Duration> {
        let json = self.json();
        (json.pointer("/error/code").and_then(Value::as_str) == Some("rate_limited")).then(|| {
            Duration::from_millis(
                json.pointer("/error/retry_after_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(1000),
            )
        })
    }
}

/// Whether a call is asked again while it does not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    /// A host busy with the person's changes answers `429`, and the same call
    /// works after the wait it names.
    Busy,
    /// [`Self::Busy`], and an action that waits for the person answers `202`:
    /// asking again under the same idempotency key asks about that same
    /// invocation, which is how a caller awaits it.
    ForPerson,
}

/// Tilo's API, as one key of the person's.
pub struct Client {
    transport: Transport,
}

enum Transport {
    /// An API server, reached over HTTPS with a key.
    Http {
        origin: String,
        key: String,
        http: tilo_net::Client,
    },
    /// A Tilo host, reached in process as this computer's own key.
    Direct(tilo_api::direct::Direct),
}

impl Client {
    /// The client `reach` names: its key file, else `TILO_API_KEY` at an API
    /// server, else this computer's own key as `tilo login` kept it.
    pub fn open(reach: Reach<'_>) -> Result<Self> {
        if let Some(path) = reach.key_file {
            let file = tilo_api::direct::KeyFile::read(path).map_err(anyhow::Error::msg)?;
            return Self::direct(&file);
        }
        if let Some(key) = api_key() {
            return Ok(Self {
                transport: Transport::Http {
                    origin: api_url(reach.api),
                    key,
                    http: tilo_net::Client::new(),
                },
            });
        }
        if let Some(file) = login_key(None)? {
            return Self::direct(&file);
        }
        bail!(
            "no key: set {KEY_ENV} (`tilo keys create \"<what will use it>\"` asks you for one), \
             or log in with `tilo login` to act as this computer"
        )
    }

    fn direct(file: &tilo_api::direct::KeyFile) -> Result<Self> {
        let direct = tilo_api::direct::Direct::new(file)
            .map_err(|why| anyhow!("the key for {}: {why}", file.host))?;
        Ok(Self {
            transport: Transport::Direct(direct),
        })
    }

    /// `GET path` (a route with its query), as JSON.
    pub async fn get(&self, path: &str) -> Result<Value> {
        let answer = self
            .call(Method::Get, path, &[], &[], Waiting::Busy)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// `POST path` with `body` as its JSON, under a new idempotency key: the
    /// answer.
    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let said = body.to_string();
        let answer = self
            .call(Method::Post, path, said.as_bytes(), &[], Waiting::Busy)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// `DELETE path`, under a new idempotency key: the answer.
    pub async fn delete(&self, path: &str) -> Result<Value> {
        let answer = self
            .call(Method::Delete, path, &[], &[], Waiting::Busy)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// Run `action` on `resource` in `workspace`, titled `title` in History:
    /// its answer, which is `pending` when the person has not answered it yet.
    pub async fn act(
        &self,
        workspace: &str,
        resource: &str,
        action: &str,
        args: &Value,
        title: &str,
    ) -> Result<Value> {
        let path = format!(
            "/v1/workspaces/{workspace}/resources/{}/actions/{action}",
            encode_path(resource)
        );
        let said = args.to_string();
        let headers = titled(title);
        let answer = self
            .call(
                Method::Post,
                &path,
                said.as_bytes(),
                &headers,
                Waiting::ForPerson,
            )
            .await?;
        if !answer.ok() && answer.status != 202 {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// Run `program` as one step of `workspace`, titled `title` in History:
    /// its answer.
    pub async fn run_program(&self, workspace: &str, program: &str, title: &str) -> Result<Value> {
        let path = format!("/v1/workspaces/{workspace}/programs");
        let body = serde_json::json!({"program": program}).to_string();
        let headers = titled(title);
        let answer = self
            .call(
                Method::Post,
                &path,
                body.as_bytes(),
                &headers,
                Waiting::Busy,
            )
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// Put `bytes` into `workspace`: the `tilo.content:` reference actions
    /// take a file by.
    pub async fn upload(&self, workspace: &str, bytes: Vec<u8>) -> Result<Value> {
        let path = format!("/v1/workspaces/{workspace}/uploads");
        let headers = [("content-type", "application/octet-stream".to_owned())];
        let answer = self
            .call(Method::Post, &path, &bytes, &headers, Waiting::Busy)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// The bytes behind the `tilo.content:` reference `content` in `workspace`.
    pub async fn content(&self, workspace: &str, content: &str) -> Result<Vec<u8>> {
        let path = format!(
            "/v1/workspaces/{workspace}/content/{}",
            encode_path(content)
        );
        let answer = self
            .call(Method::Get, &path, &[], &[], Waiting::Busy)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.body)
    }

    /// `method` on `path` with `body`, asking again as `waiting` says while
    /// there is time. A mutating call keeps one idempotency key across its
    /// retries, so a host that ran it answers what it answered the first time
    /// instead of running it again.
    async fn call(
        &self,
        method: Method,
        path: &str,
        body: &[u8],
        extra: &[(&'static str, String)],
        waiting: Waiting,
    ) -> Result<Answer> {
        let mut headers: Vec<(&'static str, String)> =
            vec![("accept", "application/json".to_owned())];
        let named = |header: &str| extra.iter().any(|(name, _)| *name == header);
        if !body.is_empty() && !named("content-type") {
            headers.push(("content-type", "application/json".to_owned()));
        }
        if method.mutating() && !named("idempotency-key") {
            headers.push(("idempotency-key", fresh_key()));
        }
        headers.extend(extra.iter().map(|(name, value)| (*name, value.clone())));
        // An action asks the server to hold the call while it waits for the
        // person, so most answers come back settled.
        if waiting == Waiting::ForPerson {
            headers.push(("prefer", format!("wait={WAIT_SECONDS}")));
        }
        let busy_until = Instant::now().checked_add(BUSY_DEADLINE);
        let answered_until = (waiting == Waiting::ForPerson)
            .then(|| Instant::now().checked_add(ANSWER_DEADLINE))
            .flatten();
        let mut told = false;
        loop {
            let answer = self.send(method, path, &headers, body).await?;
            if matches!(answer.status, 429 | 503) && ahead(busy_until) {
                let pause = answer.busy_for().unwrap_or_else(|| Duration::from_secs(1));
                tokio::time::sleep(pause).await;
                continue;
            }
            if waiting == Waiting::ForPerson && answer.status == 202 && ahead(answered_until) {
                if !told {
                    told = true;
                    let json = answer.json();
                    if let Some(url) = json.pointer("/ask/url").and_then(Value::as_str) {
                        eprintln!("tilo: waiting for you: approve it at {url}");
                    }
                }
                tokio::time::sleep(ANSWER_PAUSE).await;
                continue;
            }
            return Ok(answer);
        }
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        headers: &[(&'static str, String)],
        body: &[u8],
    ) -> Result<Answer> {
        match &self.transport {
            Transport::Http { origin, key, http } => {
                let url = format!("{origin}{path}");
                let mut request = match method {
                    Method::Get => http.get(&url),
                    Method::Post => http.post(&url),
                    Method::Delete => http.delete(&url),
                }
                .bearer_auth(key)
                .timeout(REQUEST_TIMEOUT);
                for (name, value) in headers {
                    request = request.header(*name, value.as_str());
                }
                let response = request
                    .body(body.to_vec())
                    .send()
                    .await
                    .with_context(|| format!("reaching {origin}"))?;
                let status = response.status().as_u16();
                Ok(Answer {
                    status,
                    body: response.bytes_limited(MAX_ANSWER).await?.to_vec(),
                })
            }
            Transport::Direct(direct) => {
                let pairs: Vec<(String, String)> = headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()))
                    .collect();
                let reply = direct.request(method.as_str(), path, &pairs, body).await;
                let status = reply.status().as_u16();
                let body = reply
                    .into_body()
                    .collect()
                    .await
                    .context("reading the answer")?
                    .to_bytes()
                    .to_vec();
                Ok(Answer { status, body })
            }
        }
    }
}

/// The headers a call of a run of its own carries: its id, which is the
/// idempotency key too, and the words History names it by.
fn titled(title: &str) -> [(&'static str, String); 3] {
    let id = fresh_key();
    [
        ("tilo-run", id.clone()),
        ("tilo-run-title", title.to_owned()),
        ("idempotency-key", id),
    ]
}

/// Whether `until` is still ahead: a call with no deadline never runs out.
fn ahead(until: Option<Instant>) -> bool {
    until.is_none_or(|until| Instant::now() < until)
}

/// A key one invocation is named by, and retried under.
fn fresh_key() -> String {
    hex::encode(rand::random::<[u8; 16]>())
}

/// The key `TILO_API_KEY` holds.
fn api_key() -> Option<String> {
    std::env::var(KEY_ENV)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// The API server `api` names, else `TILO_API_URL`, else [`DEFAULT_API_URL`].
pub fn api_url(api: Option<&str>) -> String {
    api.map(str::to_owned)
        .or_else(|| {
            std::env::var(API_URL_ENV)
                .ok()
                .filter(|api| !api.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_API_URL.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

/// This computer's own key as a key file ([`tilo_api::direct::KeyFile`]),
/// named `name` or as the person's devices name this computer: the key
/// `tilo login` kept, its certificate in the canonical bytes the in-process
/// API reads, and the host it reaches.
///
/// `None` when this computer did not log in.
pub fn login_key(name: Option<&str>) -> Result<Option<tilo_api::direct::KeyFile>> {
    let dir = crate::login::device_dir()?;
    let Some((key, device)) = crate::login::load(&dir)? else {
        return Ok(None);
    };
    // The login keeps the certificate as postcard bytes; the key file holds
    // the canonical encoding of the same certificate, which is what `Direct`
    // reads back.
    let cert: tilo_user_identity::DeviceCert = postcard::from_bytes(
        &hex::decode(&device.cert)
            .map_err(|_| anyhow!("this computer's certificate is not hex; log in again"))?,
    )
    .map_err(|error| anyhow!("this computer's certificate: {error}; log in again"))?;
    let canonical = tilo_user_identity::canonical::canonical_bytes(&cert)
        .map_err(|error| anyhow!("this computer's certificate: {error}"))?;
    Ok(Some(tilo_api::direct::KeyFile {
        host: device.host_origin.clone(),
        name: name.map_or_else(|| device.label.clone(), str::to_owned),
        key: hex::encode(key.to_bytes()),
        cert: hex::encode(canonical),
    }))
}

/// What a path segment leaves as it is: letters, digits and `-._~`.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `path`, its segments percent-encoded one by one: a name's spaces, `#` and
/// `?` reach the host as they are (`notes/Q3 4%20plan`), and `/` stays the
/// separator the workspace names resources by.
pub fn encode_path(path: &str) -> String {
    path.split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| percent_encoding::utf8_percent_encode(segment, SEGMENT).to_string())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_encoded_a_segment_at_a_time() {
        assert_eq!(encode_path("pdf-viewer"), "pdf-viewer");
        assert_eq!(encode_path("notes/Q3 4 plan"), "notes/Q3%204%20plan");
        assert_eq!(encode_path("a b/c#d"), "a%20b/c%23d");
        assert_eq!(encode_path("/notes/"), "notes");
    }

    #[test]
    fn a_refusal_names_what_went_wrong_and_what_to_do() {
        let answer = Answer {
            status: 403,
            body: serde_json::json!({"error": {"code": "beyond_scope",
                "message": "this key does not reach that workspace",
                "next_steps": ["GET /v1/workspaces names the workspaces it reaches"]}})
            .to_string()
            .into_bytes(),
        };
        assert_eq!(
            answer.refused().to_string(),
            "this key does not reach that workspace (beyond_scope)\n  \
             GET /v1/workspaces names the workspaces it reaches"
        );
        let bare = Answer {
            status: 500,
            body: Vec::new(),
        };
        assert_eq!(
            bare.refused().to_string(),
            "the API refused the call (HTTP 500)"
        );
    }

    #[test]
    fn only_a_busy_host_says_how_long_to_wait() {
        let busy = Answer {
            status: 429,
            body: serde_json::json!({"error": {"code": "rate_limited", "retry_after_ms": 480}})
                .to_string()
                .into_bytes(),
        };
        assert_eq!(busy.busy_for(), Some(Duration::from_millis(480)));
        let refused = Answer {
            status: 422,
            body: serde_json::json!({"error": {"code": "refused"}})
                .to_string()
                .into_bytes(),
        };
        assert_eq!(refused.busy_for(), None);
    }

    /// This computer's own key, as `tilo login` kept it, is a key file the API
    /// in process reads: its certificate in the canonical bytes `Direct`
    /// decodes, which is not the postcard the login itself keeps it as.
    /// `tilo keys export` writes the same file, for this account only.
    #[test]
    fn the_login_key_is_a_key_file_the_in_process_api_reads() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
        let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
        let now = tilo_clock::unix_now_ns();
        let month = 30 * 24 * 3600 * 1_000_000_000_u64;
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "Ada's laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            // A minute of slack, as the login's own certificates have: the
            // clock that verifies it is not the clock that issued it.
            issued_at_ns: now.saturating_sub(60_000_000_000),
            expires_at_ns: Some(now.saturating_add(month)),
            signature: [0; 64],
            issuer: None,
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root).unwrap();
        let device = crate::login::Device {
            site: "https://tilo.sh/".into(),
            host_origin: "https://eu1.tilo.test".into(),
            workspace: "6f1c2b9e-1c1f-4f0e-9a51-3c4d5e6f7a8b".into(),
            workspace_name: Some("Studio".into()),
            user: hex::encode(root.verifying_key().to_bytes()),
            agent: "Claude Code".into(),
            label: "Claude Code on build-box".into(),
            cert: hex::encode(postcard::to_allocvec(&cert).unwrap()),
            host_cert: "00".into(),
        };
        crate::login::store(dir.path(), &key, &device).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_DEVICE_DIR", dir.path());

        let file = login_key(None)
            .unwrap()
            .expect("a logged-in computer has a key");
        assert_eq!(file.host, "https://eu1.tilo.test");
        assert_eq!(file.name, "Claude Code on build-box");
        assert_eq!(file.key, hex::encode(key.to_bytes()));
        assert_eq!(
            hex::decode(&file.cert).unwrap(),
            tilo_user_identity::canonical::canonical_bytes(&cert).unwrap(),
            "the certificate is the canonical encoding the in-process API reads"
        );
        // It is a key the API in process takes, as it stands.
        tilo_api::direct::Direct::new(&file).unwrap();

        // And `tilo keys export` writes it, named, readable by this account.
        let out = dir.path().join("visa.json");
        crate::hosted::keys(
            Reach {
                api: None,
                key_file: None,
                json: false,
            },
            None,
            crate::hosted::KeysCmd::Export {
                out: out.clone(),
                name: Some("Visa backend".into()),
            },
        );
        assert_eq!(
            std::fs::metadata(&out).unwrap().permissions().mode() & 0o777,
            0o600,
            "only this account may read a key"
        );
        let written = tilo_api::direct::KeyFile::read(&out).unwrap();
        assert_eq!(written.name, "Visa backend");
        assert_eq!(written.cert, file.cert);
        tilo_api::direct::Direct::new(&written).unwrap();
    }
}
