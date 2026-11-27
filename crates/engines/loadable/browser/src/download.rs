//! One download: what an address serves, fetched into a folder under the name the page suggested,
//! telling how far it has got as it goes.
//!
//! The page that offered the download hands over what its own request would have carried (the
//! site's cookies for that address, the page it came from, the browser's user agent), so a file
//! behind a sign-in downloads here as it would have in the page. The bytes go to a hidden partial
//! file beside the final one, and take their name only once they are all there, under a name
//! nothing in the folder holds yet: a download never replaces a file. A stop or a failure removes
//! the partial file.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::Deserialize;
use tilo_net::{Client, NetError, StatusCode, Url, header};
use tokio::sync::Notify;

/// How long a connection may send nothing before the download counts as stalled.
const STALL: Duration = Duration::from_secs(60);
/// How long reaching the site may take.
const CONNECT: Duration = Duration::from_secs(30);
/// The smallest step a download of unknown size is told at.
const UNKNOWN_STEP: u64 = 1024 * 1024;

/// What an app asks to download: the address, the name the page suggested, and the handle the
/// page's window kept the request's context under.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Ask {
    /// The address to fetch.
    pub url: String,
    /// The name the page suggested; the server's or the address's when empty.
    #[serde(default)]
    pub name: String,
    /// The one-use handle of the request's context the page's window kept in this process;
    /// empty for an address the person typed.
    #[serde(default)]
    pub request: String,
    /// What the page's own request would have carried, redeemed from `request`: never read
    /// from the ask itself, which an app writes and a record may keep.
    #[serde(skip)]
    pub context: Context,
}

/// What the page's own request for a download would have carried.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Context {
    /// The page the download came from, sent as its `Referer`.
    #[serde(default)]
    pub referrer: String,
    /// The site's cookies for this address, as a `Cookie` header's value.
    #[serde(default)]
    pub cookies: String,
    /// The browser's user agent, so the site answers as it would the page.
    #[serde(default)]
    pub user_agent: String,
}

impl Ask {
    /// The ask a job's input says, with an address a download can fetch. Its context is the one
    /// `redeem` gives for its handle.
    ///
    /// # Errors
    ///
    /// The input does not read, names no web address, or names a handle that no longer holds
    /// the page's context.
    pub fn read(
        input: &str,
        redeem: impl FnOnce(&str) -> Option<String>,
    ) -> Result<(Self, Url), String> {
        let mut ask: Self = serde_json::from_str(input)
            .map_err(|error| format!("the ask does not read: {error}"))?;
        let url = Url::parse(ask.url.trim())
            .map_err(|_| format!("“{}” is not a web address", ask.url.trim()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(format!("“{url}” is not a web address a download can fetch"));
        }
        if !ask.request.is_empty() {
            let kept = redeem(&ask.request).ok_or_else(|| {
                "The page that offered this download has closed: download it from the page again"
                    .to_owned()
            })?;
            ask.context = serde_json::from_str(&kept)
                .map_err(|error| format!("the page's request does not read: {error}"))?;
        }
        Ok((ask, url))
    }
}

/// Each move a download makes, as its job tells it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Move {
    /// `done` bytes of `total` are saved; `total` is 0 while the size is unknown.
    Running { done: u64, total: u64 },
    /// The whole file is saved at `path`, called `name`.
    Done { path: PathBuf, name: String },
    /// The download stopped, for the reason said.
    Failed { error: String },
    /// The download was stopped, and what it had saved is gone.
    Cancelled,
}

impl Move {
    /// The job state's word for this move.
    #[must_use]
    pub fn state(&self) -> &'static str {
        match self {
            Self::Running { .. } => "running",
            Self::Done { .. } => "done",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// The move's own fields, as a JSON object.
    #[must_use]
    pub fn detail(&self) -> String {
        match self {
            Self::Running { done, total } => serde_json::json!({ "done": done, "total": total }),
            Self::Done { path, name } => {
                serde_json::json!({ "path": path.to_string_lossy(), "name": name })
            }
            Self::Failed { error } => serde_json::json!({ "why": error }),
            Self::Cancelled => serde_json::json!({}),
        }
        .to_string()
    }
}

/// How a download is stopped from outside, at once, even while it waits on the network.
#[derive(Clone, Default)]
pub struct Stop(Arc<(AtomicBool, Notify)>);

impl Stop {
    /// Stop the download.
    pub fn stop(&self) {
        self.0.0.store(true, Ordering::SeqCst);
        self.0.1.notify_waiters();
    }

    fn stopped(&self) -> bool {
        self.0.0.load(Ordering::SeqCst)
    }

    /// Resolves once the download is stopped.
    async fn wait(&self) {
        loop {
            let notified = self.0.1.notified();
            if self.stopped() {
                return;
            }
            notified.await;
        }
    }
}

/// Fetch `url` as `ask` says into `folder`, telling each move to `tell`, until it is saved, it
/// fails, or `stop` stops it. The last move told is the one answered.
pub fn run(
    ask: &Ask,
    url: &Url,
    folder: &Path,
    stop: &Stop,
    tell: &mut (dyn FnMut(&Move) + Send),
) -> Move {
    tell(&Move::Running { done: 0, total: 0 });
    let ended = match tilo_rt::block_on(fetch(ask, url, folder, stop, tell)) {
        Ok(Ok(saved)) => saved,
        Ok(Err(Ended::Stopped)) => Move::Cancelled,
        Ok(Err(Ended::Failed(error))) => Move::Failed { error },
        Err(error) => Move::Failed {
            error: format!("The download could not start: {error}"),
        },
    };
    tell(&ended);
    ended
}

/// Why a download ended before its file was whole.
enum Ended {
    Stopped,
    Failed(String),
}

impl From<io::Error> for Ended {
    fn from(error: io::Error) -> Self {
        Self::Failed(saving(&error))
    }
}

/// What a person reads when the file could not be written.
fn saving(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::StorageFull => "There is no room left in Downloads".to_owned(),
        io::ErrorKind::PermissionDenied => "Downloads cannot be written to".to_owned(),
        _ => format!("The file could not be saved in Downloads: {error}"),
    }
}

/// What a person reads when the site did not send the file.
fn refused(host: &str, status: StatusCode) -> String {
    let code = status.as_u16();
    match code {
        401 | 403 => format!("{host} refused the download ({code})"),
        404 | 410 => format!("{host} no longer has this file ({code})"),
        429 => format!("{host} is busy; try again in a moment ({code})"),
        500..=599 => format!("{host} had a problem sending the file ({code})"),
        _ => format!("{host} answered {code} instead of the file"),
    }
}

/// What a person reads when the network failed them.
fn unreached(host: &str, error: &NetError) -> String {
    match error {
        NetError::Timeout => format!("{host} took too long to answer"),
        NetError::Connect { .. } => format!("{host} could not be reached"),
        _ => format!("The connection to {host} was lost"),
    }
}

/// Wait for `work` unless `stop` stops the download first.
async fn or_stopped<T>(stop: &Stop, work: impl Future<Output = T>) -> Result<T, Ended> {
    tokio::select! {
        () = stop.wait() => Err(Ended::Stopped),
        done = work => Ok(done),
    }
}

async fn fetch(
    ask: &Ask,
    url: &Url,
    folder: &Path,
    stop: &Stop,
    tell: &mut (dyn FnMut(&Move) + Send),
) -> Result<Move, Ended> {
    let host = url.host_str().unwrap_or("The site").to_owned();
    let client = Client::builder()
        .connect_timeout(CONNECT)
        .build()
        .map_err(|error| Ended::Failed(format!("The download could not start: {error}")))?;
    let mut request = client.get(url.as_str());
    if !ask.context.cookies.is_empty() {
        request = request.header(header::COOKIE, ask.context.cookies.as_str());
    }
    if !ask.context.referrer.is_empty() {
        request = request.header(header::REFERER, ask.context.referrer.as_str());
    }
    if !ask.context.user_agent.is_empty() {
        request = request.header(header::USER_AGENT, ask.context.user_agent.as_str());
    }
    let mut response = or_stopped(stop, request.send())
        .await?
        .map_err(|error| Ended::Failed(unreached(&host, &error)))?;
    if !response.status().is_success() {
        return Err(Ended::Failed(refused(&host, response.status())));
    }
    // A body sent compressed arrives decompressed, so its declared length is not the file's.
    let encoded = response
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|coding| coding.as_bytes() != b"identity");
    let total = if encoded {
        0
    } else {
        response.content_length().unwrap_or(0)
    };
    let name = file_name(ask, &response);
    let partial = Partial::create(folder, &name)?;
    tell(&Move::Running { done: 0, total });
    let mut done: u64 = 0;
    let mut told: u64 = 0;
    loop {
        let chunk = match or_stopped(stop, tokio::time::timeout(STALL, response.chunk())).await? {
            Err(_) => return Err(Ended::Failed(format!("{host} stopped sending the file"))),
            Ok(Err(error)) => return Err(Ended::Failed(unreached(&host, &error))),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => chunk,
        };
        partial.file().write_all(&chunk)?;
        done = done.saturating_add(chunk.len() as u64);
        if due(told, done, total) {
            told = done;
            tell(&Move::Running { done, total });
        }
    }
    if total > 0 && done < total {
        return Err(Ended::Failed(format!("The connection to {host} was lost")));
    }
    if told != done || total == 0 {
        tell(&Move::Running { done, total: done });
    }
    let path = partial.finish(folder, &name)?;
    let name = path.file_name().map_or_else(
        || name.clone(),
        |saved| saved.to_string_lossy().into_owned(),
    );
    Ok(Move::Done { path, name })
}

/// Whether a download at `done` bytes, last told at `told`, has moved far enough to tell again:
/// each hundredth of a known size, or each twentieth of what it already has (at least a
/// mebibyte) when the size is unknown.
fn due(told: u64, done: u64, total: u64) -> bool {
    let hundredths = |bytes: u64| bytes.saturating_mul(100).checked_div(total);
    match (hundredths(told), hundredths(done)) {
        (Some(before), Some(now)) => now > before,
        _ => done.saturating_sub(told) >= UNKNOWN_STEP.max(told / 20),
    }
}

/// The name the file takes: the page's suggestion, else the server's, else the address's last
/// part, made safe to stand in one folder.
fn file_name(ask: &Ask, response: &tilo_net::Response) -> String {
    let server = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(disposition_name);
    let from_address = response
        .url()
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .map(|last| {
            percent_encoding::percent_decode_str(last)
                .decode_utf8_lossy()
                .into_owned()
        });
    [Some(ask.name.clone()), server, from_address]
        .into_iter()
        .flatten()
        .map(|name| safe_name(&name))
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "download".to_owned())
}

/// The file name a `Content-Disposition` value gives: its UTF-8 `filename*` first.
fn disposition_name(value: &str) -> Option<String> {
    let mut plain = None;
    for part in value.split(';').map(str::trim) {
        let Some((key, given)) = part.split_once('=') else {
            continue;
        };
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                let encoded = given.trim().splitn(3, '\'').nth(2)?;
                return Some(
                    percent_encoding::percent_decode_str(encoded)
                        .decode_utf8_lossy()
                        .into_owned(),
                );
            }
            "filename" => plain = Some(given.trim().trim_matches('"').to_owned()),
            _ => {}
        }
    }
    plain
}

/// `name` with no folder in it, nothing a file manager hides, and nothing no file may hold.
fn safe_name(name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    clean.trim().trim_start_matches('.').trim().to_owned()
}

/// The hidden file a download writes until it is whole, removed unless it is finished.
struct Partial {
    path: PathBuf,
    file: Option<File>,
}

impl Partial {
    fn create(folder: &Path, name: &str) -> Result<Self, Ended> {
        fs::create_dir_all(folder)?;
        for number in 0..1000_u32 {
            let path = folder.join(format!(".{name}.{number}.part"));
            match File::create_new(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Ended::Failed(format!(
            "Downloads holds too many unfinished downloads of {name}"
        )))
    }

    fn file(&self) -> &File {
        #[allow(
            clippy::expect_used,
            reason = "the file is only taken by `finish`, which consumes the partial"
        )]
        self.file
            .as_ref()
            .expect("an unfinished partial holds its file")
    }

    /// The whole file, under the first name `name` takes in `folder` that nothing holds.
    fn finish(mut self, folder: &Path, name: &str) -> Result<PathBuf, Ended> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        let names = host_fs::transfer::Names::new(name.as_ref());
        for number in 1..10_000 {
            let target = folder.join(names.for_suffix(number));
            match host_fs::rename_noreplace(&self.path, &target) {
                Ok(()) => {
                    self.path = PathBuf::new();
                    return Ok(target);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        Err(Ended::Failed(format!(
            "Downloads holds too many files called {name}"
        )))
    }
}

impl Drop for Partial {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
#[path = "download_tests.rs"]
mod tests;
