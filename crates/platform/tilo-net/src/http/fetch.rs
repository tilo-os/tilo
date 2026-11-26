//! One download from a trusted origin: the exact bytes, whole, verified.
//!
//! Bytes go into a file the caller already opened and owns, from wherever that
//! file currently ends, so an interrupted download resumes instead of starting
//! again. The response must come from the trusted origin, and every redirect
//! hop is checked against it and bounded. The body must be framed and encoded
//! in a way whose length means the bytes themselves: no transfer coding beyond
//! a single HTTP/1.1 `chunked`, and no content coding other than `identity`,
//! because a digest covers the bytes and not their compression.
//!
//! A download tries a bounded number of times, and only for failures that can
//! differ next time: connection and timeout errors, `429`, and `5xx`. A short
//! body, a refused range and an interrupted read resume from the bytes already
//! held; a mismatched size, digest or framing is final. The caller's cancel
//! flag stops it at any point, and its optional pause flag stops it where the
//! held bytes can be written down for a later run.
//!
//! [`Hooks`] is how a caller that must survive a crash writes down what it has
//! proven: it receives every chunk after it was written and every boundary
//! where the held bytes are about to change state.

use reqwest::{Url, blocking::Response, header};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Largest piece of a response body read at once.
const BUFFER: usize = 64 * 1024;
/// Attempts one download makes before it reports itself incomplete.
const ATTEMPTS: usize = 3;
/// Redirects one request follows before it refuses the chain.
const REDIRECTS: usize = 5;
/// Interrupted file writes one chunk survives before they are a failure.
const INTERRUPTED_WRITES: u8 = 8;
/// How long connecting to the origin may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What went wrong, in terms a caller can map onto what it tells a person.
#[derive(Debug)]
pub enum Error {
    /// The caller's cancel flag was set.
    Cancelled,
    /// The caller's pause flag was set, after [`Hooks::pausing`] ran.
    Paused,
    /// A URL, redirect or response left the trusted origin's rules.
    Trusted(String),
    /// A response or the caller's file broke the download's bounds.
    Protocol(String),
    /// The download could not finish in its bounded attempts.
    Incomplete(String),
    /// The held bytes were not the expected size.
    Size { expected: u64, found: u64 },
    /// The held bytes were not the expected SHA-256.
    Digest,
    /// Reading, seeking or sizing the caller's file failed.
    Io(io::Error),
    /// A [`Hooks`] method refused.
    Hook(String),
    /// The HTTP client could not be built.
    Client(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("the download was cancelled"),
            Self::Paused => formatter.write_str("the download was paused"),
            Self::Trusted(detail) => write!(formatter, "untrusted download: {detail}"),
            Self::Protocol(detail) => write!(formatter, "refused response: {detail}"),
            Self::Incomplete(detail) => write!(formatter, "incomplete download: {detail}"),
            Self::Size { expected, found } => {
                write!(formatter, "downloaded {found} bytes instead of {expected}")
            }
            Self::Digest => formatter.write_str("the download did not match its digest"),
            Self::Io(error) => write!(formatter, "download file: {error}"),
            Self::Hook(detail) => write!(formatter, "download hook: {detail}"),
            Self::Client(detail) => write!(formatter, "HTTP client: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

/// Where a download is, and how many bytes of the file are in hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Deciding what to ask for next.
    Requesting,
    /// Appending the response body.
    Receiving,
    /// Asking again after a failure that can differ next time.
    Retrying,
    /// Discarding the held bytes and asking for the whole file.
    Resetting,
    /// Hashing the whole file against its expected digest.
    Verifying,
    /// The file holds exactly the expected, verified bytes.
    Complete,
}

/// One report from a download.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub phase: Phase,
    /// Bytes held in the file. While [`Phase::Verifying`] runs the file already
    /// holds all of them, so this is the finished size.
    pub received: u64,
    /// The exact size the finished file has.
    pub total: u64,
}

/// What a download is asked for.
pub struct Request<'a> {
    /// The absolute URL on the downloader's trusted origin.
    pub url: Url,
    /// The exact number of bytes the finished file holds.
    pub size: u64,
    /// SHA-256 of the finished file, as lowercase hex, when the caller knows it.
    pub sha256: Option<&'a str>,
    pub cancel: &'a AtomicBool,
    /// Stops at the next boundary where the held bytes can be written down.
    pub pause: Option<&'a AtomicBool>,
}

/// What a caller does with the bytes as they arrive.
///
/// Every method may refuse, which ends the download with [`Error::Hook`]; a
/// caller that has written nothing refuses rather than continue with a file it
/// cannot vouch for. `file` is the caller's working file, positioned at the
/// end of what it already wrote.
pub trait Hooks {
    /// An attempt starts, with `offset` bytes already in the file. Whatever the
    /// caller knows about those bytes can be checked now, before anything is
    /// sent for them.
    fn attempt(&mut self, _file: &mut File, _offset: u64) -> Result<(), String> {
        Ok(())
    }

    /// Append `bytes` to the file. The default appends them with the write
    /// retries a filesystem needs; a caller that records a recoverable
    /// checkpoint writes them and records where they end.
    fn write(&mut self, file: &mut File, bytes: &[u8]) -> Result<(), String> {
        append(file, bytes)
    }

    /// The response ended short of the file, and the next request resumes from
    /// the held bytes: make them durable before it is sent.
    fn before_retry(&mut self, _file: &mut File) -> Result<(), String> {
        Ok(())
    }

    /// The held bytes are about to be discarded.
    fn before_reset(&mut self, _file: &mut File) -> Result<(), String> {
        Ok(())
    }

    /// The file is empty again.
    fn reset(&mut self, _file: &mut File) -> Result<(), String> {
        Ok(())
    }

    /// The caller asked to pause: settle the held bytes before returning
    /// [`Error::Paused`].
    fn pausing(&mut self, _file: &mut File) -> Result<(), String> {
        Ok(())
    }

    /// The whole file matched its expected size and digest.
    fn verified(&mut self, _file: &mut File) -> Result<(), String> {
        Ok(())
    }
}

impl Hooks for () {}

/// Append `bytes` to `file`, retrying interrupted writes, and fail rather than
/// report an append that only partly happened.
pub fn append(file: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    let mut remaining = bytes;
    let mut interrupted: u8 = 0;
    while !remaining.is_empty() {
        let count = match file.write(remaining) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interrupted = interrupted.checked_add(1).ok_or_else(|| {
                    "the file write was interrupted more often than it can be".to_string()
                })?;
                if interrupted >= INTERRUPTED_WRITES {
                    return Err(format!(
                        "the file write was interrupted {interrupted} times"
                    ));
                }
                continue;
            }
            Err(error) => return Err(format!("the file write failed: {error}")),
        };
        if count == 0 {
            return Err(format!(
                "the file write accepted no bytes: {}",
                io::Error::from(io::ErrorKind::WriteZero)
            ));
        }
        interrupted = 0;
        remaining = remaining
            .get(count..)
            .ok_or_else(|| "the file write reported more bytes than it was given".to_string())?;
    }
    Ok(())
}

/// Whether `actual` names the same origin as `trusted`: one scheme, host and
/// port, with no credentials, query or fragment to make it a different address.
#[must_use]
pub fn same_origin(actual: &Url, trusted: &Url) -> bool {
    actual.scheme() == trusted.scheme()
        && actual.host_str() == trusted.host_str()
        && actual.port_or_known_default() == trusted.port_or_known_default()
        && actual.username().is_empty()
        && actual.password().is_none()
        && actual.query().is_none()
        && actual.fragment().is_none()
}

/// One trusted origin's downloads.
pub struct Downloader {
    client: reqwest::blocking::Client,
    origin: Url,
}

impl Downloader {
    /// A downloader for `origin`. `timeout` bounds one request including its
    /// body, so a download that outlasts it resumes where it stopped rather
    /// than fail.
    pub fn new(origin: Url, timeout: Duration, user_agent: Option<&str>) -> Result<Self, Error> {
        let mut builder = super::blocking_client()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(timeout)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .retry(reqwest::retry::never())
            .redirect(reqwest::redirect::Policy::none());
        if let Some(user_agent) = user_agent {
            builder = builder.user_agent(user_agent);
        }
        let client = builder
            .build()
            .map_err(|error| Error::Client(error.to_string()))?;
        Ok(Self { client, origin })
    }

    /// Download `request` into `file`, resuming from its current length, and
    /// leave it holding exactly `size` verified bytes.
    #[allow(
        clippy::too_many_lines,
        reason = "one attempt's steps read in the order they run: ask, read, settle"
    )]
    pub fn fetch(
        &self,
        request: Request<'_>,
        file: &mut File,
        hooks: &mut impl Hooks,
        mut progress: impl FnMut(Progress),
    ) -> Result<(), Error> {
        let Request {
            url,
            size,
            sha256,
            cancel,
            pause,
        } = request;
        stop(cancel)?;
        check_size(size)?;
        if !same_origin(&url, &self.origin) {
            return Err(Error::Trusted(
                "the download URL is not on the trusted origin".into(),
            ));
        }
        let mut last = "no response was received".to_owned();
        for attempt in 0..ATTEMPTS {
            stop(cancel)?;
            if paused(pause) {
                hooks.pausing(file).map_err(Error::Hook)?;
                return Err(Error::Paused);
            }
            let offset = held_length(file, size, cancel)?;
            hooks.attempt(file, offset).map_err(Error::Hook)?;
            if held_length(file, size, cancel)? != offset {
                return Err(Error::Protocol(
                    "the held bytes changed while an attempt started".into(),
                ));
            }
            if offset == size {
                return verify(file, size, sha256, cancel, pause, hooks, &mut progress);
            }
            report(&mut progress, Phase::Requesting, offset, size);
            let mut response = match self.request(url.clone(), offset, cancel, pause) {
                Ok(response) => response,
                Err(RequestError::Paused) => {
                    hooks.pausing(file).map_err(Error::Hook)?;
                    return Err(Error::Paused);
                }
                Err(RequestError::Fatal(detail)) => {
                    stop(cancel)?;
                    return Err(Error::Trusted(detail));
                }
                Err(RequestError::Retry(detail)) => {
                    stop(cancel)?;
                    last = detail;
                    report(&mut progress, Phase::Retrying, offset, size);
                    continue;
                }
            };
            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                stop(cancel)?;
                last = format!("the origin answered {status}");
                report(&mut progress, Phase::Retrying, offset, size);
                continue;
            }
            let expected = match expected(&response, offset, size) {
                Ok(Expected::Bytes(count)) => count,
                Ok(Expected::Refused(detail)) => {
                    stop(cancel)?;
                    last = detail.into();
                    discard(file, size, cancel, pause, hooks, &mut progress)?;
                    continue;
                }
                Err(error) => {
                    stop(cancel)?;
                    return Err(error);
                }
            };
            stop(cancel)?;
            file.seek(SeekFrom::Start(offset)).map_err(Error::Io)?;
            let mut written = offset;
            report(&mut progress, Phase::Receiving, written, size);
            let end = read_body(
                &mut response,
                expected,
                cancel,
                || paused(pause),
                |chunk| {
                    hooks.write(file, chunk).map_err(Error::Hook)?;
                    let length = held_length(file, size, cancel)?;
                    if written.checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                        != Some(length)
                    {
                        return Err(Error::Protocol(
                            "a chunk was not written at the end of the file".into(),
                        ));
                    }
                    written = length;
                    report(&mut progress, Phase::Receiving, written, size);
                    Ok(())
                },
            )?;
            drop(response);
            let length = held_length(file, size, cancel)?;
            match end {
                BodyEnd::Complete if length == size => {
                    return verify(file, size, sha256, cancel, pause, hooks, &mut progress);
                }
                BodyEnd::Paused => {
                    hooks.pausing(file).map_err(Error::Hook)?;
                    return Err(Error::Paused);
                }
                BodyEnd::Complete => {
                    last = "the response ended before the file was complete".into();
                }
                BodyEnd::Interrupted(detail) => last = detail,
            }
            if length == size {
                // The response did not finish, so its bytes are not trusted even
                // though they fill the file: fetch the whole file again.
                discard(file, size, cancel, pause, hooks, &mut progress)?;
            } else if attempt.saturating_add(1) < ATTEMPTS && length > offset {
                hooks.before_retry(file).map_err(Error::Hook)?;
            }
            report(&mut progress, Phase::Retrying, length, size);
        }
        stop(cancel)?;
        if paused(pause) {
            hooks.pausing(file).map_err(Error::Hook)?;
            return Err(Error::Paused);
        }
        Err(Error::Incomplete(format!(
            "after {ATTEMPTS} attempts: {last}"
        )))
    }

    /// Download `request` whole into memory. A manifest is small and always
    /// fetched again, so this resumes nothing.
    pub fn fetch_bytes(&self, request: Request<'_>) -> Result<Vec<u8>, Error> {
        let Request {
            url,
            size,
            sha256,
            cancel,
            pause,
        } = request;
        stop(cancel)?;
        check_size(size)?;
        if !same_origin(&url, &self.origin) {
            return Err(Error::Trusted(
                "the download URL is not on the trusted origin".into(),
            ));
        }
        let mut response = match self.request(url, 0, cancel, pause) {
            Ok(response) => response,
            Err(RequestError::Paused) => return Err(Error::Paused),
            Err(RequestError::Fatal(detail)) => return Err(Error::Trusted(detail)),
            Err(RequestError::Retry(detail)) => return Err(Error::Incomplete(detail)),
        };
        let expected = match expected(&response, 0, size) {
            Ok(Expected::Bytes(count)) => count,
            Ok(Expected::Refused(detail)) => return Err(Error::Protocol(detail.into())),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        let end = read_body(
            &mut response,
            expected,
            cancel,
            || paused(pause),
            |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        )?;
        match end {
            BodyEnd::Complete => {}
            BodyEnd::Paused => return Err(Error::Paused),
            BodyEnd::Interrupted(detail) => return Err(Error::Incomplete(detail)),
        }
        stop(cancel)?;
        if bytes.len() as u64 != size {
            return Err(Error::Size {
                expected: size,
                found: bytes.len() as u64,
            });
        }
        if sha256.is_some_and(|expected| !digest_matches(&bytes, expected)) {
            return Err(Error::Digest);
        }
        stop(cancel)?;
        Ok(bytes)
    }

    /// One response for `url`, following at most [`REDIRECTS`] same-origin
    /// redirects, with its origin and framing already checked.
    fn request(
        &self,
        mut url: Url,
        offset: u64,
        cancel: &AtomicBool,
        pause: Option<&AtomicBool>,
    ) -> Result<Response, RequestError> {
        for hop in 0..=REDIRECTS {
            cancelled(cancel).map_err(RequestError::Fatal)?;
            if paused(pause) {
                return Err(RequestError::Paused);
            }
            if !same_origin(&url, &self.origin) {
                return Err(RequestError::Fatal(
                    "the request URL is not on the trusted origin".into(),
                ));
            }
            let mut request = self
                .client
                .get(url.clone())
                .header(header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let sent = request.send();
            cancelled(cancel).map_err(RequestError::Fatal)?;
            if paused(pause) {
                return Err(RequestError::Paused);
            }
            let response = sent.map_err(|error| {
                // A header or protocol error is not a transport failure that can
                // differ next time; only connecting and timeouts are retried.
                if error.is_connect() || error.is_timeout() {
                    RequestError::Retry(error.to_string())
                } else {
                    RequestError::Fatal(error.to_string())
                }
            })?;
            identity(&self.origin, &response).map_err(RequestError::Fatal)?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let locations: Vec<_> = response
                .headers()
                .get_all(header::LOCATION)
                .iter()
                .collect();
            if hop == REDIRECTS || locations.len() != 1 {
                return Err(RequestError::Fatal(
                    "invalid redirect or too many redirects".into(),
                ));
            }
            let location = locations
                .first()
                .ok_or_else(|| RequestError::Fatal("invalid redirect Location".into()))?
                .to_str()
                .map_err(|_| RequestError::Fatal("invalid redirect Location".into()))?;
            url = url
                .join(location)
                .map_err(|_| RequestError::Fatal("invalid redirect URL".into()))?;
            if !same_origin(&url, &self.origin) {
                return Err(RequestError::Fatal(
                    "the redirect left the trusted origin".into(),
                ));
            }
        }
        Err(RequestError::Fatal(
            "the redirect chain exceeded its bound".into(),
        ))
    }
}

/// What one response offers a download that holds `offset` bytes.
enum Expected {
    /// Read this many body bytes from `offset`.
    Bytes(u64),
    /// The origin did not honour the requested range.
    Refused(&'static str),
}

/// The bytes a response promises, checked against what was asked for.
fn expected(response: &Response, offset: u64, size: u64) -> Result<Expected, Error> {
    let status = response.status();
    if offset > 0 {
        if status == reqwest::StatusCode::PARTIAL_CONTENT {
            return Ok(match range_size(response.headers(), offset, size) {
                Some(count) => Expected::Bytes(count),
                None => Expected::Refused("the origin sent a range the request did not ask for"),
            });
        }
        if status == reqwest::StatusCode::OK || status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
        {
            return Ok(Expected::Refused(
                "the origin ignored or rejected the requested range",
            ));
        }
        return Err(Error::Protocol(format!(
            "unexpected status {status} for a resumed download"
        )));
    }
    if status != reqwest::StatusCode::OK {
        return Err(Error::Protocol(format!(
            "the first response must be 200, got {status}"
        )));
    }
    declared_length(response.headers(), size).map_err(Error::Protocol)?;
    if response.headers().contains_key(header::CONTENT_RANGE) {
        return Err(Error::Protocol(
            "a full response carried a Content-Range".into(),
        ));
    }
    Ok(Expected::Bytes(size))
}

/// Discard the held bytes and settle that decision with the caller's hooks, so
/// the next request asks for the whole file.
fn discard(
    file: &mut File,
    total: u64,
    cancel: &AtomicBool,
    pause: Option<&AtomicBool>,
    hooks: &mut impl Hooks,
    progress: &mut impl FnMut(Progress),
) -> Result<(), Error> {
    stop(cancel)?;
    if paused(pause) {
        hooks.pausing(file).map_err(Error::Hook)?;
        return Err(Error::Paused);
    }
    hooks.before_reset(file).map_err(Error::Hook)?;
    file.set_len(0).map_err(Error::Io)?;
    file.rewind().map_err(Error::Io)?;
    hooks.reset(file).map_err(Error::Hook)?;
    report(progress, Phase::Resetting, 0, total);
    stop(cancel)
}

/// Hash the whole file and check it against the size and digest it was
/// downloaded for.
fn verify(
    file: &mut File,
    size: u64,
    sha256: Option<&str>,
    cancel: &AtomicBool,
    pause: Option<&AtomicBool>,
    hooks: &mut impl Hooks,
    progress: &mut impl FnMut(Progress),
) -> Result<(), Error> {
    stop(cancel)?;
    if paused(pause) {
        hooks.pausing(file).map_err(Error::Hook)?;
        return Err(Error::Paused);
    }
    report(progress, Phase::Verifying, size, size);
    file.flush().map_err(Error::Io)?;
    stop(cancel)?;
    file.rewind().map_err(Error::Io)?;
    let mut hash = Sha256::new();
    let end = read_body(
        file,
        size,
        cancel,
        || paused(pause),
        |chunk| {
            hash.update(chunk);
            Ok(())
        },
    )?;
    match end {
        BodyEnd::Complete => {}
        BodyEnd::Paused => {
            hooks.pausing(file).map_err(Error::Hook)?;
            return Err(Error::Paused);
        }
        BodyEnd::Interrupted(detail) => return Err(Error::Incomplete(detail)),
    }
    stop(cancel)?;
    if held_length(file, size, cancel)? != size {
        return Err(Error::Protocol(
            "the held file changed while it was verified".into(),
        ));
    }
    let found = hex::encode(hash.finalize());
    if sha256.is_some_and(|expected| !found.eq_ignore_ascii_case(expected)) {
        return Err(Error::Digest);
    }
    report(progress, Phase::Complete, size, size);
    stop(cancel)?;
    if paused(pause) {
        hooks.pausing(file).map_err(Error::Hook)?;
        return Err(Error::Paused);
    }
    hooks.verified(file).map_err(Error::Hook)?;
    stop(cancel)?;
    if paused(pause) {
        hooks.pausing(file).map_err(Error::Hook)?;
        return Err(Error::Paused);
    }
    Ok(())
}

/// What a request failed as: settled, worth another try, or paused.
enum RequestError {
    Paused,
    Fatal(String),
    Retry(String),
}

fn report(progress: &mut impl FnMut(Progress), phase: Phase, received: u64, total: u64) {
    progress(Progress {
        phase,
        received,
        total,
    });
}

fn stop(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Acquire) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("the download was cancelled".into())
    } else {
        Ok(())
    }
}

fn paused(pause: Option<&AtomicBool>) -> bool {
    pause.is_some_and(|pause| pause.load(Ordering::Acquire))
}

fn check_size(size: u64) -> Result<(), Error> {
    if size == 0 {
        Err(Error::Protocol("a download has a nonzero size".into()))
    } else {
        Ok(())
    }
}

fn digest_matches(bytes: &[u8], expected: &str) -> bool {
    hex::encode(Sha256::digest(bytes)).eq_ignore_ascii_case(expected)
}

fn held_length(file: &File, size: u64, cancel: &AtomicBool) -> Result<u64, Error> {
    stop(cancel)?;
    let metadata = file.metadata().map_err(Error::Io)?;
    stop(cancel)?;
    if !metadata.is_file() || metadata.len() > size {
        return Err(Error::Protocol(
            "the download must be written to a regular file no longer than its expected size"
                .into(),
        ));
    }
    Ok(metadata.len())
}

/// The declared length of a response, when it declares one, is exactly
/// `expected`; two declarations are always ambiguous.
fn declared_length(headers: &header::HeaderMap, expected: u64) -> Result<(), String> {
    let values: Vec<_> = headers.get_all(header::CONTENT_LENGTH).iter().collect();
    if values.len() > 1
        || values
            .first()
            .is_some_and(|value| value.to_str().ok().and_then(decimal) != Some(expected))
    {
        return Err("unexpected or ambiguous Content-Length".into());
    }
    Ok(())
}

fn decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        None
    } else {
        value.parse().ok()
    }
}

/// The bytes a `206` response carries, checked against what the request asked
/// for: it starts where the file ends, stays inside the file, declares the
/// whole file's size, and its `Content-Length` agrees with its range.
fn range_size(headers: &header::HeaderMap, offset: u64, total: u64) -> Option<u64> {
    let values: Vec<_> = headers.get_all(header::CONTENT_RANGE).iter().collect();
    if values.len() != 1 {
        return None;
    }
    let value = values.first()?.to_str().ok()?.strip_prefix("bytes ")?;
    let (range, full) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, full) = (decimal(start)?, decimal(end)?, decimal(full)?);
    if start != offset || end < start || end >= total || full != total {
        return None;
    }
    let count = end.checked_sub(start)?.checked_add(1)?;
    declared_length(headers, count).ok()?;
    Some(count)
}

/// The response came from the trusted origin, and its framing means its bytes
/// are the body: one HTTP/1.1 `chunked` transfer coding with no length beside
/// it, or no transfer coding at all, and no content coding but `identity`.
fn identity(origin: &Url, response: &Response) -> Result<(), String> {
    if !same_origin(response.url(), origin) {
        return Err("the response came from an untrusted origin".into());
    }
    let mut framing = response.headers().get_all(header::TRANSFER_ENCODING).iter();
    if let Some(value) = framing.next() {
        let ambiguous = framing.next().is_some()
            || response.version() != reqwest::Version::HTTP_11
            || response.headers().contains_key(header::CONTENT_LENGTH)
            || !value
                .to_str()
                .is_ok_and(|value| value.trim().eq_ignore_ascii_case("chunked"));
        if ambiguous {
            return Err("unsupported or ambiguous Transfer-Encoding framing".into());
        }
    }
    let encodings: Vec<_> = response
        .headers()
        .get_all(header::CONTENT_ENCODING)
        .iter()
        .collect();
    if encodings.len() > 1 || encodings.first().is_some_and(|value| **value != "identity") {
        return Err("unsupported or ambiguous Content-Encoding".into());
    }
    Ok(())
}

/// How a body ended.
enum BodyEnd {
    /// Exactly the expected bytes were read.
    Complete,
    /// The caller asked to pause.
    Paused,
    /// The response ended or failed before the expected bytes arrived.
    Interrupted(String),
}

/// Read exactly `expected` bytes, allowing one more so a body that overruns is
/// noticed rather than trusted.
fn read_body(
    reader: &mut impl Read,
    expected: u64,
    cancel: &AtomicBool,
    pause: impl Fn() -> bool,
    mut chunk: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<BodyEnd, Error> {
    let mut buffer = vec![0_u8; BUFFER];
    let mut received = 0_u64;
    loop {
        stop(cancel)?;
        if pause() {
            return Ok(BodyEnd::Paused);
        }
        let remaining = expected.saturating_sub(received);
        let limit =
            usize::try_from(remaining.saturating_add(1).min(BUFFER as u64)).unwrap_or(BUFFER);
        let target = buffer
            .get_mut(..limit)
            .ok_or_else(|| Error::Protocol("a read bound exceeded its buffer".into()))?;
        let read = reader.read(target);
        stop(cancel)?;
        if pause() {
            return Ok(BodyEnd::Paused);
        }
        let count = match read {
            Ok(count) => count,
            Err(error) => {
                return Ok(BodyEnd::Interrupted(format!(
                    "the response read failed: {error}"
                )));
            }
        };
        if count == 0 {
            return Ok(if received == expected {
                BodyEnd::Complete
            } else {
                BodyEnd::Interrupted("the response was truncated".into())
            });
        }
        let allowed = usize::try_from(remaining).unwrap_or(usize::MAX);
        if count > allowed {
            return Err(Error::Protocol(
                "the response exceeded its expected byte boundary".into(),
            ));
        }
        let got = buffer
            .get(..count)
            .ok_or_else(|| Error::Protocol("a read exceeded its buffer".into()))?;
        chunk(got)?;
        stop(cancel)?;
        received = received.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }
}
