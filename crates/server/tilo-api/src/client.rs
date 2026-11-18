//! Tilo's API, `/v1`, as a program of the person's reaches it (`tilo`'s
//! hosted commands, `tilolang repl`): one client, two transports, and nothing
//! that knows which kind of server answers.
//!
//! `docs/developers/api.md` is the callers' reference. A call goes to an API
//! server over HTTP with a key (`TILO_API_KEY`, and the server named by
//! `--api` or `TILO_API_URL`), or to a Tilo host in process
//! ([`crate::direct::Direct`]) as the key file `--key-file` names. Every route
//! answers the same either way, so no command here can tell. A Tilo desktop
//! on this computer serves the same routes to its own person's programs on a
//! socket of its login ([`Client::session`]), and so does the process holding
//! the session of the device `tilo login` made of this computer
//! ([`device::held_api`]): a server keeps one session per device, and a
//! program entering as the device on its own would cut that one off.
//!
//! A mutating call carries an `Idempotency-Key` of its own, and every retry of
//! it sends that same key: Tilo never runs one invocation twice.

pub mod device;
mod model_asks;

use anyhow::{Context as _, Result, anyhow, bail};
use http_body_util::BodyExt as _;
use serde_json::Value;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

pub const DEFAULT_API_URL: &str = "https://mcp.tilo.sh";
pub const API_URL_ENV: &str = "TILO_API_URL";
pub const KEY_ENV: &str = "TILO_API_KEY";
const MAX_ANSWER: usize = 64 * 1024 * 1024;
/// The longest one request is held before the server answers that it still
/// waits: `Prefer: wait=` takes seconds, 60 at most.
const WAIT_SECONDS: &str = "60";
/// How long a call keeps asking while the server says it is unavailable.
const RETRY_DEADLINE: Duration = Duration::from_secs(60);
/// How long an action waits for the person to answer it.
const ANSWER_DEADLINE: Duration = Duration::from_secs(300);
/// How long a request is given, uploads included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
/// How long a call waits before asking about a pending run again.
const ANSWER_PAUSE: Duration = Duration::from_secs(1);
/// The most one piece of an upload carries: a file larger than this goes up
/// in as many pieces as it takes, each a request the server reads as it
/// arrives.
const UPLOAD_PIECE: usize = 8 * 1024 * 1024;
/// The most one request fetches of a file: a download of a large file reads
/// it a piece at a time, so the whole of it is never held at once.
const DOWNLOAD_PIECE: usize = 32 * 1024 * 1024;
/// How many pieces in a row may fail in transport before an upload or a
/// download gives up.
/// Each failure is resumed from wherever the server says the upload stands,
/// so the bytes already sent are never sent again.
const PIECE_TRIES: usize = 5;

/// A request's method, as the routes of the API take them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Delete,
}

impl Method {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
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

impl Reach<'_> {
    /// Whether these calls act as the device `tilo login` made of this
    /// computer — the one transport that knows the workspace it entered. A
    /// key file and an API key are someone else's key, and name no workspace.
    fn as_device(self) -> bool {
        self.key_file.is_none() && api_key().is_none()
    }
}

/// The workspace this computer's login entered, which `-w` means when nobody
/// named one: its id, and the name the device keeps for it. `None` when these
/// calls act as some other key, or this computer is not logged in.
pub fn entered_workspace(reach: Reach<'_>) -> Result<Option<(String, String)>> {
    if !reach.as_device() {
        return Ok(None);
    }
    device::entered_workspace()
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
        let code = field("code").and_then(Value::as_str).unwrap_or("failed");
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
        // The server names its own routes and an agent's tools; a person at
        // this command types neither.
        let said = crate::v1::names::said(&said, crate::v1::names::Surface::Cli);
        anyhow::Error::new(Refusal {
            code: code.to_owned(),
            message: said,
            retry_after_ms: field("retry_after_ms").and_then(Value::as_u64),
        })
    }
}

/// A call the API refused: the code it answered, its words, and the wait it
/// asked for before the same call works.
#[derive(Clone, Debug)]
pub struct Refusal {
    code: String,
    message: String,
    retry_after_ms: Option<u64>,
}

impl Refusal {
    /// The API's refusal among `error`'s causes, when it refused.
    #[must_use]
    pub fn of(error: &anyhow::Error) -> Option<&Self> {
        error.downcast_ref()
    }

    /// The API's stable code for why it refused.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// How long the API asked the caller to wait before asking again.
    #[must_use]
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.retry_after_ms
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Refusal {}

/// Whether a call is asked again while it does not answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    /// The server says it is briefly unavailable (`503`): the same call works
    /// once it is back.
    Transient,
    /// [`Self::Transient`], and an action that waits for the person answers `202`:
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
    /// A Tilo server, reached in process as this computer's own key.
    Direct(crate::direct::Direct),
    /// A Tilo desktop on this computer, at the socket it serves its person's
    /// programs on, as that desktop's own device.
    Session(std::path::PathBuf),
}

impl Client {
    /// The client `reach` names: this desktop's own API at its socket when
    /// one serves it; this computer's own device as `tilo login` made it, at
    /// the socket of the process holding its session, when `reach` names no
    /// key or names that device's own ([`held_api_for`]); else its key file;
    /// else `TILO_API_KEY` at an API server.
    pub fn open(reach: Reach<'_>) -> Result<Self> {
        // A desktop of this login serves its person at its own socket, and a
        // program of this login answers as the person there with no key of its
        // own: the socket's directory and permissions and the kernel's account
        // of the peer are what admit it ([`desktop_socket`]). A server named
        // outright, and any key, still decide.
        if reach.api.is_none()
            && reach.key_file.is_none()
            && api_key().is_none()
            && let Some(socket) = desktop_socket()
        {
            return Ok(Self::session(&socket));
        }
        if let Some(socket) = held_api_for(reach)? {
            return Ok(Self::session(&socket));
        }
        if let Some(path) = reach.key_file {
            let file = crate::direct::KeyFile::read(path).map_err(anyhow::Error::msg)?;
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
        bail!(
            "no key: set {KEY_ENV} (`tilo keys create \"<what will use it>\"` asks you for one), \
             or log in with `tilo login` to act as this computer"
        )
    }

    /// The client of the Tilo desktop serving its person's programs at
    /// `socket` ([`tilo_paths::login::API_SOCKET_FILE`] in its login's
    /// session directory).
    #[must_use]
    pub fn session(socket: &Path) -> Self {
        Self {
            transport: Transport::Session(socket.to_path_buf()),
        }
    }

    fn direct(file: &crate::direct::KeyFile) -> Result<Self> {
        let direct = crate::direct::Direct::new(file)
            .map_err(|why| anyhow!("the key for {}: {why}", file.host))?;
        Ok(Self {
            transport: Transport::Direct(direct),
        })
    }

    /// `GET path` (a route with its query), as JSON.
    pub async fn get(&self, path: &str) -> Result<Value> {
        let answer = self
            .call(Method::Get, path, &[], &[], Waiting::Transient)
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
            .call(Method::Post, path, said.as_bytes(), &[], Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// `PUT path` with `body` as its JSON, under a new idempotency key: the
    /// answer.
    pub async fn put(&self, path: &str, body: &Value) -> Result<Value> {
        let said = body.to_string();
        let answer = self
            .call(Method::Put, path, said.as_bytes(), &[], Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// `PUT path` with `body` as its JSON, under a new idempotency key, held
    /// while the person decides: the answer, `pending` when they have not
    /// answered yet.
    pub async fn put_asking(&self, path: &str, body: &Value) -> Result<Value> {
        let said = body.to_string();
        let answer = self
            .call(Method::Put, path, said.as_bytes(), &[], Waiting::ForPerson)
            .await?;
        if !answer.ok() && answer.status != 202 {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// `DELETE path`, under a new idempotency key: the answer.
    pub async fn delete(&self, path: &str) -> Result<Value> {
        let answer = self
            .call(Method::Delete, path, &[], &[], Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// The workspace `named`: an id as it is, or the account's workspace of
    /// that name.
    pub async fn workspace_id(&self, named: &str) -> Result<String> {
        if is_uuid(named) {
            return Ok(named.to_owned());
        }
        let listed = self.get("/v1/workspaces").await?;
        let wanted = slug(named);
        listed
            .get("workspaces")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|workspace| {
                workspace
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| slug(name) == wanted)
            })
            .and_then(|workspace| workspace.get("id").and_then(Value::as_str))
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("no workspace called {named:?}: `tilo workspace ls` names them"))
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

    /// Run `action` on `resource` in `workspace` as a step of the invocation
    /// `run`, titled `title` in History, under the idempotency key `key`: its
    /// answer, which is `pending` when the person has not answered it yet.
    ///
    /// Every call under one `run` is grouped in History as one change, and
    /// every retry of a step goes with the same `key`, so a retried step runs
    /// once.
    pub async fn act_invoked(
        &self,
        workspace: &str,
        resource: &str,
        action: &str,
        args: &Value,
        run: &str,
        title: &str,
        key: &str,
    ) -> Result<Value> {
        let path = format!(
            "/v1/workspaces/{workspace}/resources/{}/actions/{action}",
            encode_path(resource)
        );
        let said = args.to_string();
        let mut headers = grouped(run, title).to_vec();
        headers.push(("idempotency-key", key.to_owned()));
        // An action asks the server to hold the call while it runs, so most
        // answers come back settled.
        headers.push(("prefer", format!("wait={WAIT_SECONDS}")));
        let answer = self
            .call(
                Method::Post,
                &path,
                said.as_bytes(),
                &headers,
                Waiting::Transient,
            )
            .await?;
        if !answer.ok() && answer.status != 202 {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// The invocation `run` in `workspace`, held while it is pending for up
    /// to `wait` seconds: its answer, `pending` while it still is.
    pub async fn invocation(&self, workspace: &str, run: &str, wait: u64) -> Result<Value> {
        let path = format!(
            "/v1/workspaces/{workspace}/invocations/{}?wait={wait}",
            encode_path(run)
        );
        let answer = self
            .call(Method::Get, &path, &[], &[], Waiting::Transient)
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
                Waiting::Transient,
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
            .call(Method::Post, &path, &bytes, &headers, Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// Which of `hashes` (lowercase BLAKE3 hex) `workspace` already holds, in
    /// order: the contents it kept from an earlier put need no upload again.
    pub async fn have_contents(&self, workspace: &str, hashes: &[String]) -> Result<Vec<bool>> {
        let path = format!("/v1/workspaces/{workspace}/contents/have");
        let body = serde_json::json!({"hashes": hashes}).to_string();
        let answer = self
            .call(
                Method::Post,
                &path,
                body.as_bytes(),
                &[],
                Waiting::Transient,
            )
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        let said = answer.json();
        let have: Vec<bool> = said
            .get("have")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| row.as_bool().unwrap_or_default())
                    .collect()
            })
            .unwrap_or_default();
        if have.len() != hashes.len() {
            bail!(
                "the workspace answered for {} of {} content hashes",
                have.len(),
                hashes.len()
            );
        }
        Ok(have)
    }

    /// Store `files` whole in `workspace` in one request, answering each one's
    /// `tilo.content:` reference in order: a folder of small files without a
    /// request each.
    pub async fn put_contents(&self, workspace: &str, files: &[Vec<u8>]) -> Result<Vec<String>> {
        use base64::Engine as _;
        let encoded: Vec<String> = files
            .iter()
            .map(|bytes| base64::engine::general_purpose::STANDARD.encode(bytes))
            .collect();
        let path = format!("/v1/workspaces/{workspace}/contents");
        let body = serde_json::json!({"files": encoded}).to_string();
        let answer = self
            .call(Method::Put, &path, body.as_bytes(), &[], Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        let said = answer.json();
        let contents: Vec<String> = said
            .get("contents")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        if contents.len() != files.len() {
            bail!(
                "the workspace answered for {} of {} files",
                contents.len(),
                files.len()
            );
        }
        Ok(contents)
    }

    /// Put the file at `path` into `workspace`, one request when it fits in
    /// one and in pieces when it does not, telling `told` how many of its
    /// bytes are there after each: the `tilo.content:` reference actions take
    /// a file by.
    ///
    /// A piece that fails in transport is sent again from wherever the
    /// workspace says the upload stands, so a broken connection resumes
    /// instead of starting the file again.
    pub async fn upload_file(
        &self,
        workspace: &str,
        path: &Path,
        told: &mut impl FnMut(u64, u64),
    ) -> Result<Value> {
        let size = std::fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .len();
        told(0, size);
        if size <= UPLOAD_PIECE as u64 {
            let bytes =
                std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            let answer = self.upload(workspace, bytes).await?;
            told(size, size);
            return Ok(answer);
        }
        // The id is this side's, so an upload this command opened can be
        // asked about—and finished—after a connection it lost.
        let id = format!("up_{}", fresh_key());
        let upload = format!("/v1/workspaces/{workspace}/uploads/{id}");
        let opened = self
            .post(&upload, &serde_json::json!({"size": size}))
            .await?;
        let mut offset = opened.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let mut file =
            std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut piece = vec![0_u8; UPLOAD_PIECE];
        let mut failed = 0_usize;
        told(offset, size);
        while offset < size {
            let left = usize::try_from(size.saturating_sub(offset).min(UPLOAD_PIECE as u64))
                .unwrap_or(UPLOAD_PIECE);
            let Some(wanting) = piece.get_mut(..left) else {
                bail!("{} cannot be read in pieces", path.display());
            };
            file.read_exact(wanting)
                .with_context(|| format!("reading {}", path.display()))?;
            match self.upload_piece(&upload, offset, wanting).await {
                Ok(answer) => {
                    failed = 0;
                    if answer.get("content").is_some() {
                        told(size, size);
                        return Ok(answer);
                    }
                    offset = answer
                        .get("offset")
                        .and_then(Value::as_u64)
                        .unwrap_or(offset);
                    told(offset, size);
                }
                Err(why) if failed < PIECE_TRIES => {
                    failed = failed.saturating_add(1);
                    // Where does it stand now? The server keeps an upload a
                    // lost connection cut, so the rest continues it.
                    let status = self.upload_status(&upload).await?;
                    offset = status
                        .get("offset")
                        .and_then(Value::as_u64)
                        .unwrap_or(offset);
                    told(offset, size);
                    if failed == PIECE_TRIES {
                        return Err(why.context(format!(
                            "{} stopped {failed} times, the last at {offset} of {size} bytes",
                            path.display()
                        )));
                    }
                }
                Err(why) => return Err(why),
            }
            file.seek(SeekFrom::Start(offset))?;
        }
        bail!(
            "the upload of {} finished without naming its content",
            path.display()
        )
    }

    /// Send `piece`, the next bytes of `upload`, which this side believes
    /// stands at `offset`: a piece the host already has is refused rather
    /// than stored twice.
    async fn upload_piece(&self, upload: &str, offset: u64, piece: &[u8]) -> Result<Value> {
        let headers = [("content-type", "application/octet-stream".to_owned())];
        let path = format!("{upload}?offset={offset}");
        let answer = self
            .call(Method::Put, &path, piece, &headers, Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// Where `upload` stands: how many of its bytes the workspace has.
    pub async fn upload_status(&self, upload: &str) -> Result<Value> {
        let answer = self
            .call(Method::Get, upload, &[], &[], Waiting::Transient)
            .await?;
        if !answer.ok() {
            return Err(answer.refused());
        }
        Ok(answer.json())
    }

    /// The bytes behind the `tilo.content:` reference `content` in
    /// `workspace`, written to `output` as they come: a file of any size goes
    /// through, a piece at a time, telling `told` how many bytes are written
    /// and how many the file is when the answer says.
    #[expect(
        clippy::future_not_send,
        reason = "the writer is this command's own, used on its one thread"
    )]
    pub async fn download(
        &self,
        workspace: &str,
        content: &str,
        output: &mut dyn std::io::Write,
        told: &mut impl FnMut(u64, Option<u64>),
    ) -> Result<u64> {
        let path = format!(
            "/v1/workspaces/{workspace}/content/{}",
            encode_path(content)
        );
        let mut written = 0_u64;
        let mut failed = 0_usize;
        loop {
            // The same range asked again answers the same bytes: a piece the
            // host was too busy to answer is asked for again, and a file of
            // hundreds of megabytes is not lost to one slow answer.
            let end = written
                .saturating_add(DOWNLOAD_PIECE as u64)
                .saturating_sub(1);
            let headers = [("range", format!("bytes={written}-{end}"))];
            let answer = match self
                .call(Method::Get, &path, &[], &headers, Waiting::Transient)
                .await
            {
                Ok(answer) => {
                    failed = 0;
                    answer
                }
                Err(why) if failed < PIECE_TRIES => {
                    failed = failed.saturating_add(1);
                    tokio::time::sleep(Duration::from_millis(
                        250_u64.saturating_mul(u64::try_from(failed).unwrap_or(1)),
                    ))
                    .await;
                    if failed == PIECE_TRIES {
                        return Err(why.context(format!("the download stopped at {written} bytes")));
                    }
                    continue;
                }
                Err(why) => return Err(why),
            };
            if !answer.ok() {
                return Err(answer.refused());
            }
            if answer.body.is_empty() {
                break;
            }
            let got = answer.body.len() as u64;
            output.write_all(&answer.body).context("writing the file")?;
            written = written.saturating_add(got);
            told(written, None);
            // A piece shorter than asked for is the end of the file.
            if got < DOWNLOAD_PIECE as u64 {
                break;
            }
        }
        Ok(written)
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
        let retry_until = Instant::now().checked_add(RETRY_DEADLINE);
        let answered_until = (waiting == Waiting::ForPerson)
            .then(|| Instant::now().checked_add(ANSWER_DEADLINE))
            .flatten();
        let mut told = false;
        loop {
            let answer = self.send(method, path, &headers, body).await?;
            if answer.status == 503 && ahead(retry_until) {
                tokio::time::sleep(Duration::from_secs(1)).await;
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
        let mut opened = self.opening(method, path, headers, body).await?;
        let mut whole = Vec::new();
        while let Some(piece) = opened.piece().await? {
            if whole.len().saturating_add(piece.len()) > MAX_ANSWER {
                bail!("the answer is larger than {MAX_ANSWER} bytes");
            }
            whole.extend_from_slice(&piece);
        }
        Ok(Answer {
            status: opened.status,
            body: whole,
        })
    }

    /// `method` on `path`: its status, and its body to read as it comes.
    async fn opening(
        &self,
        method: Method,
        path: &str,
        headers: &[(&'static str, String)],
        body: &[u8],
    ) -> Result<Opened> {
        match &self.transport {
            Transport::Http { origin, key, http } => {
                let url = format!("{origin}{path}");
                let mut request = match method {
                    Method::Get => http.get(&url),
                    Method::Post => http.post(&url),
                    Method::Put => http.put(&url),
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
                Ok(Opened {
                    status: response.status().as_u16(),
                    body: Pieces::Net(Box::new(response)),
                })
            }
            Transport::Direct(direct) => {
                let pairs: Vec<(String, String)> = headers
                    .iter()
                    .map(|(name, value)| ((*name).to_owned(), value.clone()))
                    .collect();
                // Boxed as a trait object: the whole server answers in this future, deeper than
                // a caller's own future may nest or the compiler follows it to prove it `Send`.
                let request: std::pin::Pin<
                    Box<dyn std::future::Future<Output = crate::Reply> + Send + '_>,
                > = Box::pin(direct.request(method.as_str(), path, &pairs, body));
                let reply = request.await;
                Ok(Opened {
                    status: reply.status().as_u16(),
                    body: Pieces::Direct(reply.into_body()),
                })
            }
            Transport::Session(socket) => {
                let reached = || format!("reaching Tilo on this computer at {}", socket.display());
                let stream = tilo_files::native::socket::connect(socket).with_context(reached)?;
                stream.set_nonblocking(true).with_context(reached)?;
                let stream = tokio::net::UnixStream::from_std(stream).with_context(reached)?;
                let (mut sender, connection) =
                    hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                        .await
                        .with_context(reached)?;
                tokio::spawn(connection);
                let mut request = hyper::Request::builder().method(method.as_str()).uri(path);
                for (name, value) in headers {
                    request = request.header(*name, value.as_str());
                }
                let request = request
                    .body(http_body_util::Full::new(
                        hyper::body::Bytes::copy_from_slice(body),
                    ))
                    .context("the request")?;
                let response = tokio::time::timeout(REQUEST_TIMEOUT, sender.send_request(request))
                    .await
                    .with_context(reached)?
                    .with_context(reached)?;
                Ok(Opened {
                    status: response.status().as_u16(),
                    body: Pieces::Socket(response.into_body()),
                })
            }
        }
    }
}

/// An answer whose body is read as it comes.
struct Opened {
    status: u16,
    body: Pieces,
}

/// A body of an answer, by the transport that carries it.
enum Pieces {
    Net(Box<tilo_net::Response>),
    Direct(crate::Body),
    Socket(hyper::body::Incoming),
}

impl Opened {
    /// The body's next piece; `None` once it ended.
    async fn piece(&mut self) -> Result<Option<hyper::body::Bytes>> {
        loop {
            let frame = match &mut self.body {
                Pieces::Net(response) => {
                    return response
                        .chunk()
                        .await
                        .map_err(|error| anyhow!("reading the answer: {error}"));
                }
                Pieces::Direct(body) => match body.frame().await {
                    None => return Ok(None),
                    Some(frame) => frame.map_err(|never| match never {})?,
                },
                Pieces::Socket(body) => match body.frame().await {
                    None => return Ok(None),
                    Some(frame) => frame.map_err(|error| anyhow!("reading the answer: {error}"))?,
                },
            };
            if let Ok(data) = frame.into_data() {
                return Ok(Some(data));
            }
        }
    }
}

/// The headers grouping a call under the invocation `run`: the run, and the
/// words History names it by.
fn grouped(run: &str, title: &str) -> [(&'static str, String); 2] {
    [
        (crate::v1::INVOCATION_HEADER, run.to_owned()),
        (crate::v1::INVOCATION_TITLE_HEADER, title.to_owned()),
    ]
}

/// The headers a call of an invocation of its own carries: its id, which is the
/// idempotency key too, and the words History names it by.
fn titled(title: &str) -> Vec<(&'static str, String)> {
    let id = fresh_key();
    let mut headers = grouped(&id, title).to_vec();
    headers.push(("idempotency-key", id));
    headers
}

/// Whether `text` is written as a workspace's id is.
#[must_use]
pub fn is_uuid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.chars().all(|c| c.is_ascii_hexdigit())
        })
}

/// A name as `-w` matches it: letters and digits, lower case, words joined
/// by `-` (`Applicant: Jane Doe` and `applicant-jane-doe` match).
#[must_use]
pub fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Whether `until` is still ahead: a call with no deadline never runs out.
fn ahead(until: Option<Instant>) -> bool {
    until.is_none_or(|until| Instant::now() < until)
}

/// A key one invocation is named by, and retried under.
fn fresh_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// The key `TILO_API_KEY` holds.
fn api_key() -> Option<String> {
    std::env::var(KEY_ENV)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// The socket this login's desktop serves Tilo's API to its person's own
/// programs at, when one serves it: `API_SOCKET_FILE` in the session's
/// directory under the login's runtime directory
/// ([`tilo_paths::login::session_dir`]). A program of this login finds it by
/// the runtime directory alone, and one at another account's session never
/// does.
#[must_use]
pub fn desktop_socket() -> Option<std::path::PathBuf> {
    if let Some(named) = std::env::var_os(DESKTOP_ENV)
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Some(named);
    }
    let directory = tilo_paths::login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))?;
    let socket = directory.join(tilo_paths::login::API_SOCKET_FILE);
    // A socket that answers is one this session serves still; one a session
    // left behind is not.
    let is_socket = std::os::unix::fs::FileTypeExt::is_socket(
        &std::fs::symlink_metadata(&socket).ok()?.file_type(),
    );
    is_socket.then_some(socket)
}

/// Names the desktop's own API socket, as `TILO_MCP_SERVER` names the agent
/// socket: `TILO_API_SOCKET`. A program of a login whose runtime directory is
/// not the one the desktop was started in finds it here.
pub const DESKTOP_ENV: &str = "TILO_API_SOCKET";

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

/// This computer's own key as a key file ([`crate::direct::KeyFile`]),
/// named `name` or as the person's devices name this computer: the key
/// `tilo login` kept, its certificate in the canonical bytes the in-process
/// API reads, and the host it reaches.
///
/// `None` when this computer did not log in.
pub fn login_key(name: Option<&str>) -> Result<Option<crate::direct::KeyFile>> {
    let dir = device::device_dir()?;
    let Some((key, device)) = device::load(&dir)? else {
        return Ok(None);
    };
    // The login keeps the certificate as postcard bytes; the key file holds
    // the canonical encoding of the same certificate, which is what `Direct`
    // reads back.
    let cert: tilo_user_identity::DeviceCert = postcard::from_bytes(
        &hex::decode(&device.cert).map_err(|_| anyhow!(device::LOGIN_DAMAGED))?,
    )
    .map_err(|_| anyhow!(device::LOGIN_DAMAGED))?;
    let canonical = tilo_user_identity::canonical::canonical_bytes(&cert)
        .map_err(|_| anyhow!(device::LOGIN_DAMAGED))?;
    Ok(Some(crate::direct::KeyFile {
        host: device.host_origin.clone(),
        // The site the device logged in with: links this key asks the host
        // for are built on it, as the site's own page builds them.
        site: Some(device.site.trim_end_matches('/').to_owned()),
        name: name.map_or_else(|| device.label.clone(), str::to_owned),
        key: hex::encode(key.to_bytes()),
        cert: hex::encode(canonical),
        reached_at: match &device.server_address {
            Some(address) => Some(crate::server::ServerInfo {
                server_cert: device.certs()?.1,
                address: Some(address.clone()),
            }),
            None => None,
        },
    }))
}

/// Where calls with `reach` go to this computer's held session
/// ([`device::held_api`]): when it names no key and this computer logged in,
/// or when its key file or `TILO_API_KEY` is this computer's own device key.
/// A key has one holder: a program acting with the device's key on a session
/// of its own would cut the computer's off. `None` for any other key.
///
/// # Errors
/// The key file cannot be read, or the login is damaged.
pub fn held_api_for(reach: Reach<'_>) -> Result<Option<std::path::PathBuf>> {
    let named = match (reach.key_file, api_key()) {
        (Some(path), _) => Some(
            crate::direct::KeyFile::read(path)
                .map_err(anyhow::Error::msg)?
                .key,
        ),
        (None, Some(token)) => Some(
            crate::store::carried_key(&token)
                .map(|(_, key)| hex::encode(key.secret()))
                .unwrap_or_default(),
        ),
        (None, None) => None,
    };
    if let Some(secret) = named {
        let Some((key, _)) = device::load(&device::device_dir()?)? else {
            return Ok(None);
        };
        if !crate::constant_time_eq(
            secret.to_ascii_lowercase().as_bytes(),
            hex::encode(key.to_bytes()).as_bytes(),
        ) {
            return Ok(None);
        }
    }
    device::held_api()
}

/// The key a script was approved (`token`, as `tilo keys create` answers it),
/// as a key file reaching where this computer's login reaches: a key of its
/// own, never the device's, so the script's session and the computer's each
/// have one holder. `None` when this computer did not log in.
///
/// # Errors
/// The token is not a key's, or the login is damaged.
pub fn script_key(token: &str) -> Result<Option<crate::direct::KeyFile>> {
    let Some(file) = login_key(None)? else {
        return Ok(None);
    };
    Ok(Some(file.with_token(token).map_err(anyhow::Error::msg)?))
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

/// `value` percent-encoded for a query: everything but letters, digits and
/// `-._~`, `/` and `#` included, so the host reads it whole.
pub fn encode_query(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, SEGMENT).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn an_api_call_encodes_its_path_and_reads_its_refusals;

        phase a_path_is_encoded_a_segment_at_a_time;
        {
            assert_eq!(encode_path("pdf-viewer"), "pdf-viewer");
            assert_eq!(encode_path("notes/Q3 4 plan"), "notes/Q3%204%20plan");
            assert_eq!(encode_path("a b/c#d"), "a%20b/c%23d");
            assert_eq!(encode_path("/notes/"), "notes");
        }

        phase a_refusal_names_what_went_wrong_and_what_to_do;
        {
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

        phase a_desktop_serving_this_login_is_found_before_any_key_is_looked_for;
        {
            let root = std::env::temp_dir().join(format!("tilo-desktop-socket-{}", std::process::id()));
            std::fs::create_dir_all(&root).expect("a directory for the socket");
            let socket = root.join(tilo_paths::login::API_SOCKET_FILE);
            let _ = std::fs::remove_file(&socket);
            let listener = std::os::unix::net::UnixListener::bind(&socket).expect("a socket");
            // Named outright, so the test needs no runtime directory of its own.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(DESKTOP_ENV, &socket)
                .remove(KEY_ENV)
                .remove(API_URL_ENV);
            assert_eq!(
                desktop_socket().as_deref(),
                Some(socket.as_path()),
                "a socket this login's desktop serves is found by its name"
            );
            // No key file, no `TILO_API_KEY`, no API server named: the desktop is what is
            // reached, so opening the client is no error about a missing key. Before the socket
            // is looked for, this is the "no key" refusal, which is what main answered.
            let client = Client::open(Reach {
                api: None,
                key_file: None,
                json: false,
            });
            assert!(
                matches!(
                    client.as_ref().map(|client| &client.transport),
                    Ok(Transport::Session(found)) if found == &socket
                ),
                "a desktop's socket is reached with no key: {:?}",
                client.err().map(|error| error.to_string())
            );
            drop(listener);
            drop(env);
            std::fs::remove_dir_all(&root).expect("the socket's directory goes");
        }
    }
}
