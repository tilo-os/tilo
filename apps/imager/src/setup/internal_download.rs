//! Authenticated selection and bounded HTTP transfer through caller-owned handles.
//! This module neither owns cache paths nor publishes files. Authentication depends
//! on the embedded catalog belonging to a genuinely signed application.

use super::internal_bundle::{ComponentView, InternalBundle, MAX_MANIFEST_BYTES};
use crate::release::InternalRelease;
use reqwest::{Url, blocking::Client, header};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const BUFFER: usize = 64 * 1024;
const MAX_COMPONENT: u64 = 1 << 40;

/// Exact selected bytes and validated metadata, not stable-file/install authority.
#[derive(Debug)]
pub struct SelectedManifest {
    bytes: Vec<u8>,
    bundle: InternalBundle,
    digest: String,
    asset: Url,
}
impl SelectedManifest {
    pub fn from_bytes(release: &InternalRelease, bytes: Vec<u8>) -> Result<Self, String> {
        Self::checked(release, bytes, &production_origin())
    }
    fn checked(release: &InternalRelease, bytes: Vec<u8>, origin: &Url) -> Result<Self, String> {
        let bundle = release.verify_manifest(&bytes)?;
        let asset = asset_url(release.manifest_url(), origin)?;
        Ok(Self {
            bytes,
            bundle,
            digest: release.manifest_sha256().into(),
            asset,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn bundle(&self) -> &InternalBundle {
        &self.bundle
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.digest
    }
    fn component(&self, index: usize) -> Result<ComponentView<'_>, String> {
        self.bundle
            .component(index)
            .ok_or_else(|| "Invalid component index".into())
    }
    fn component_url(&self, component: ComponentView<'_>) -> Url {
        let mut url = self.asset.clone();
        url.path_segments_mut()
            .expect("checked hierarchical asset")
            .pop()
            .push(component.filename);
        url
    }
}
fn production_origin() -> Url {
    Url::parse("https://code.podesta.ai/").expect("literal URL")
}
fn same_origin(url: &Url, origin: &Url) -> bool {
    url.scheme() == origin.scheme()
        && url.host_str() == origin.host_str()
        && url.port_or_known_default() == origin.port_or_known_default()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
}
fn asset_url(value: &str, origin: &Url) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|e| e.to_string())?;
    if !same_origin(&url, origin)
        || url
            .path_segments()
            .and_then(|s| s.last())
            .is_none_or(str::is_empty)
    {
        return Err("Unsupported selected asset URL".into());
    }
    Ok(url)
}
fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Internal transfer cancelled".into())
    } else {
        Ok(())
    }
}
fn io_error(error: std::io::Error) -> String {
    format!("Internal working-file/stream error: {error}")
}

/// A reusable client with fixed trust policy; production has no origin override.
/// Network acquisition and each blocking read time out after 30 seconds (connect:
/// 10 seconds). This is not a whole-component deadline. Filesystem cancellation
/// is cooperative around blocking operations, with no latency guarantee.
pub struct InternalTransport {
    client: Client,
    origin: Url,
}
impl InternalTransport {
    pub fn new() -> Result<Self, String> {
        Self::client(production_origin(), Duration::from_secs(30))
    }
    fn client(origin: Url, timeout: Duration) -> Result<Self, String> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(timeout)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .retry(reqwest::retry::never())
            // Handle redirects explicitly: reqwest strips Location fragments before
            // its redirect policy callback, too late for our stricter URL contract.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client, origin })
    }
    pub fn fetch_manifest(
        &self,
        release: &InternalRelease,
        cancel: &AtomicBool,
    ) -> Result<SelectedManifest, String> {
        cancelled(cancel)?;
        let url = asset_url(release.manifest_url(), &self.origin)?;
        let expected = release.manifest_size();
        if expected == 0 || expected > MAX_MANIFEST_BYTES as u64 {
            return Err("Invalid selected manifest size".into());
        }
        let mut response = self.request(url, 0, cancel).map_err(|e| e.message())?;
        cancelled(cancel)?;
        self.response_identity(&response)?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("Manifest response must be 200".into());
        }
        declared_length(response.headers(), expected)?;
        let mut bytes = Vec::with_capacity(expected as usize);
        if let BodyEnd::Interrupted(error) = read_body(&mut response, expected, cancel, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })? {
            return Err(error);
        }
        cancelled(cancel)?;
        let selected = SelectedManifest::checked(release, bytes, &self.origin)?;
        cancelled(cancel)?;
        Ok(selected)
    }
    fn request(
        &self,
        mut url: Url,
        offset: u64,
        cancel: &AtomicBool,
    ) -> Result<reqwest::blocking::Response, RequestError> {
        for hops in 0..=5 {
            cancelled(cancel).map_err(RequestError::Fatal)?;
            if !same_origin(&url, &self.origin) {
                return Err(RequestError::Fatal("Untrusted request URL".into()));
            }
            let mut request = self
                .client
                .get(url.clone())
                .header(header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let response = request.send();
            cancelled(cancel).map_err(RequestError::Fatal)?;
            let response = response.map_err(|e| {
                // Header/protocol parsing errors are not transient transport
                // failures. Only explicit connect/timeouts retry acquisition.
                if e.is_connect() || e.is_timeout() {
                    RequestError::Retry(e.to_string())
                } else {
                    RequestError::Fatal(e.to_string())
                }
            })?;
            self.response_identity(&response)
                .map_err(RequestError::Fatal)?;
            if !matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let locations: Vec<_> = response
                .headers()
                .get_all(header::LOCATION)
                .iter()
                .collect();
            if hops == 5 || locations.len() != 1 {
                return Err(RequestError::Fatal(
                    "Invalid redirect or hop limit exceeded".into(),
                ));
            }
            let location = locations[0]
                .to_str()
                .map_err(|_| RequestError::Fatal("Invalid redirect Location".into()))?;
            url = url
                .join(location)
                .map_err(|_| RequestError::Fatal("Invalid redirect URL".into()))?;
            if !same_origin(&url, &self.origin) {
                return Err(RequestError::Fatal(
                    "Redirect left trusted URL policy".into(),
                ));
            }
        }
        unreachable!("bounded redirect loop returns")
    }
    fn response_identity(&self, response: &reqwest::blocking::Response) -> Result<(), String> {
        if !same_origin(response.url(), &self.origin) {
            return Err("Untrusted response origin".into());
        }
        // Hyper can decode unknown transfer codings as close-delimited and
        // prefer chunked over Content-Length. Enforce our narrower policy while
        // the original response fields remain visible, before any redirect/reset.
        let mut framing = response.headers().get_all(header::TRANSFER_ENCODING).iter();
        if let Some(value) = framing.next() {
            if framing.next().is_some()
                || response.version() != reqwest::Version::HTTP_11
                || response.headers().contains_key(header::CONTENT_LENGTH)
                || !value
                    .to_str()
                    .is_ok_and(|value| value.trim().eq_ignore_ascii_case("chunked"))
            {
                return Err("Unsupported or ambiguous Transfer-Encoding framing".into());
            }
        }
        let values: Vec<_> = response
            .headers()
            .get_all(header::CONTENT_ENCODING)
            .iter()
            .collect();
        if values.len() > 1 || values.first().is_some_and(|v| **v != "identity") {
            return Err("Unsupported or ambiguous Content-Encoding".into());
        }
        Ok(())
    }
    /// `file` must be an already-open, exclusively owned, unpublished regular
    /// working file with read/write access. The caller authorizes and retains the
    /// handle; this API never discovers, replaces, deletes or publishes a path.
    /// On failure bytes remain on that handle; no returned pathname is authority.
    pub fn transfer(
        &self,
        selected: &SelectedManifest,
        index: usize,
        file: &mut File,
        checkpoint: Option<&ResumeCheckpoint>,
        cancel: &AtomicBool,
        operation: u64,
        progress: impl FnMut(Progress),
    ) -> Result<CompletedComponent, String> {
        let mut events = Events {
            callback: progress,
            operation,
            component: index,
            last: None,
            received: 0,
        };
        let result = self.transfer_inner(selected, index, file, checkpoint, cancel, &mut events);
        if result.is_err() {
            events.emit(
                if cancel.load(Ordering::Acquire) {
                    Phase::Cancelled
                } else {
                    Phase::Failed
                },
                0,
                true,
            );
        }
        result
    }
    fn transfer_inner(
        &self,
        selected: &SelectedManifest,
        index: usize,
        file: &mut File,
        checkpoint: Option<&ResumeCheckpoint>,
        cancel: &AtomicBool,
        events: &mut Events<impl FnMut(Progress)>,
    ) -> Result<CompletedComponent, String> {
        cancelled(cancel)?;
        let component = selected.component(index)?;
        if !same_origin(&selected.asset, &self.origin) {
            return Err("Selected context origin mismatch".into());
        }
        let url = selected.component_url(component);
        // Do not clone untrusted checkpoint strings before validating their bounds.
        let mut checkpoint = checkpoint.map(Cow::Borrowed);
        let mut last_error = "Incomplete response".to_owned();
        for attempt in 0..3 {
            cancelled(cancel)?;
            events.received = regular_length(file, component.byte_size, cancel)?;
            events.emit(Phase::Prefix, 0, true);
            let offset = validate_prefix(selected, component, file, checkpoint.as_deref(), cancel)?;
            if offset == component.byte_size {
                return finish(component, file, cancel, events);
            }
            cancelled(cancel)?;
            events.emit(Phase::Requesting, 0, true);
            cancelled(cancel)?;
            let mut response = match self.request(url.clone(), offset, cancel) {
                Ok(response) => response,
                Err(RequestError::Fatal(error)) => return Err(error),
                Err(RequestError::Retry(error)) => {
                    last_error = error;
                    events.emit(Phase::Retrying, 0, true);
                    continue;
                }
            };
            self.response_identity(&response)?;
            let status = response.status();
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                last_error = format!("Transient component HTTP status {status}");
                events.emit(Phase::Retrying, 0, true);
                continue;
            }
            let expected = if offset > 0 {
                if status == reqwest::StatusCode::PARTIAL_CONTENT {
                    range_size(response.headers(), offset, component.byte_size)
                } else if status == reqwest::StatusCode::OK
                    || status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE
                {
                    None
                } else {
                    return Err(format!("Unexpected resumed component HTTP status {status}"));
                }
            } else {
                if status != reqwest::StatusCode::OK {
                    return Err(format!(
                        "Initial component response must be 200, got {status}"
                    ));
                }
                declared_length(response.headers(), component.byte_size)?;
                if response.headers().contains_key(header::CONTENT_RANGE) {
                    return Err("Unexpected range on full response".into());
                }
                Some(component.byte_size)
            };
            let Some(expected) = expected else {
                last_error = "Server ignored/rejected Range or sent invalid range bounds".into();
                reset(file, cancel, events)?;
                checkpoint = None;
                continue;
            };
            cancelled(cancel)?;
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            cancelled(cancel)?;
            events.emit(Phase::Receiving, 0, true);
            let end = read_body(&mut response, expected, cancel, |chunk| {
                write_chunk(file, chunk, cancel, events)
            })?;
            let length = regular_length(file, component.byte_size, cancel)?;
            match end {
                BodyEnd::Complete if length == component.byte_size => {
                    return finish(component, file, cancel, events);
                }
                BodyEnd::Complete => {
                    last_error = "Partial range ended before complete component".into()
                }
                BodyEnd::Interrupted(error) => last_error = error,
            }
            // A full byte counter after a failed EOF is not a successful response.
            // Demand a fresh complete response within this invocation.
            if length == component.byte_size {
                reset(file, cancel, events)?;
                checkpoint = None;
            } else if attempt < 2 {
                checkpoint = Some(Cow::Owned(capture_checkpoint(
                    selected, index, file, cancel,
                )?));
            }
            events.emit(Phase::Retrying, 0, true);
        }
        cancelled(cancel)?;
        Err(format!(
            "Component incomplete after 3 attempts ({} held bytes): {last_error}",
            regular_length(file, component.byte_size, cancel)?
        ))
    }
}

enum RequestError {
    Fatal(String),
    Retry(String),
}
impl RequestError {
    fn message(self) -> String {
        match self {
            Self::Fatal(s) | Self::Retry(s) => s,
        }
    }
}

fn declared_length(headers: &header::HeaderMap, expected: u64) -> Result<(), String> {
    let values: Vec<_> = headers.get_all(header::CONTENT_LENGTH).iter().collect();
    if values.len() > 1
        || values
            .first()
            .is_some_and(|v| v.to_str().ok().and_then(decimal) != Some(expected))
    {
        return Err("Unexpected or ambiguous Content-Length".into());
    }
    Ok(())
}
fn decimal(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        None
    } else {
        value.parse().ok()
    }
}
enum BodyEnd {
    Complete,
    Interrupted(String),
}
/// Always consumes bounded EOF, including a sentinel read after the last expected byte.
fn read_body(
    reader: &mut impl Read,
    expected: u64,
    cancel: &AtomicBool,
    mut chunk: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<BodyEnd, String> {
    let mut buffer = [0; BUFFER];
    let mut received = 0;
    loop {
        cancelled(cancel)?;
        let limit = ((expected - received + 1).min(BUFFER as u64)) as usize;
        let read = reader.read(&mut buffer[..limit]);
        cancelled(cancel)?;
        let count = match read {
            Ok(n) => n,
            Err(e) => return Ok(BodyEnd::Interrupted(format!("Response read failed: {e}"))),
        };
        if count == 0 {
            return Ok(if received == expected {
                BodyEnd::Complete
            } else {
                BodyEnd::Interrupted("Response truncated".into())
            });
        }
        if count as u64 > expected - received {
            return Err("Response exceeds authenticated byte boundary".into());
        }
        chunk(&buffer[..count])?;
        cancelled(cancel)?;
        received += count as u64;
    }
}

/// Untrusted checkpoint data. Validation rehashes the entire held prefix.
#[derive(Clone, Debug)]
pub struct ResumeCheckpoint {
    pub manifest_sha256: String,
    pub role: String,
    pub filename: String,
    pub full_size: u64,
    pub full_sha256: String,
    pub prefix_size: u64,
    pub prefix_sha256: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Prefix,
    Requesting,
    Receiving,
    Retrying,
    Resetting,
    Verifying,
    Complete,
    Failed,
    Cancelled,
}
/// Progress is advisory. `verified` stays zero until the complete authenticated
/// digest matches; prefix hashes only validate checkpoint consistency. A later
/// coordinator must match the operation ID and successful result before readiness.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub operation: u64,
    pub component: usize,
    pub phase: Phase,
    pub received: u64,
    pub verified: u64,
}
/// Observed exact bytes during this invocation only. Later publication/staging must
/// retain/reacquire its own file identities and reverify these bytes.
#[derive(Debug)]
pub struct CompletedComponent {
    byte_size: u64,
}
impl CompletedComponent {
    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
}

/// Flush/sync and hash bounded held bytes for an untrusted checkpoint. This does
/// not publish a durable journal or authenticate a partial component. A later
/// cache owns reopen/identity checks, crash recovery and journal publication.
pub fn capture_checkpoint(
    selected: &SelectedManifest,
    index: usize,
    file: &mut File,
    cancel: &AtomicBool,
) -> Result<ResumeCheckpoint, String> {
    cancelled(cancel)?;
    let component = selected.component(index)?;
    let length = regular_length(file, component.byte_size, cancel)?;
    file.flush().map_err(io_error)?;
    cancelled(cancel)?;
    file.sync_all().map_err(io_error)?;
    cancelled(cancel)?;
    let digest = hash_file(file, length, cancel)?;
    cancelled(cancel)?;
    Ok(ResumeCheckpoint {
        manifest_sha256: selected.digest.clone(),
        role: component.role.into(),
        filename: component.filename.into(),
        full_size: component.byte_size,
        full_sha256: component.sha256.into(),
        prefix_size: length,
        prefix_sha256: digest,
    })
}
fn regular_length(file: &File, max: u64, cancel: &AtomicBool) -> Result<u64, String> {
    cancelled(cancel)?;
    let metadata = file.metadata().map_err(io_error)?;
    cancelled(cancel)?;
    if !metadata.is_file() || max > MAX_COMPONENT || metadata.len() > max {
        return Err("Output must be a bounded regular working file".into());
    }
    Ok(metadata.len())
}
fn hash_file(file: &mut File, length: u64, cancel: &AtomicBool) -> Result<String, String> {
    if regular_length(file, length, cancel)? != length {
        return Err("Working file length changed".into());
    }
    file.rewind().map_err(io_error)?;
    cancelled(cancel)?;
    let mut hash = Sha256::new();
    #[cfg(test)]
    let mut count = 0;
    let end = read_body(file, length, cancel, |chunk| {
        hash.update(chunk);
        #[cfg(test)]
        {
            count += chunk.len() as u64;
            tests::event("hash", count);
        }
        cancelled(cancel)
    })?;
    if let BodyEnd::Interrupted(error) = end {
        return Err(error);
    }
    #[cfg(test)]
    tests::event("hash-eof", count);
    cancelled(cancel)?;
    if regular_length(file, length, cancel)? != length {
        return Err("Working file length changed".into());
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn validate_prefix(
    selected: &SelectedManifest,
    component: ComponentView<'_>,
    file: &mut File,
    checkpoint: Option<&ResumeCheckpoint>,
    cancel: &AtomicBool,
) -> Result<u64, String> {
    let length = regular_length(file, component.byte_size, cancel)?;
    match checkpoint {
        None if length != 0 => {
            return Err("Nonempty output requires an explicit resume checkpoint".into());
        }
        None => return Ok(0),
        Some(c) => {
            if c.manifest_sha256 != selected.digest
                || c.role != component.role
                || c.filename != component.filename
                || c.full_size != component.byte_size
                || c.full_sha256 != component.sha256
                || c.prefix_size != length
                || c.prefix_size > component.byte_size
                || !super::internal_bundle::canonical_digest(&c.prefix_sha256, 64)
            {
                return Err(
                    "Resume checkpoint does not match selected component/held length".into(),
                );
            }
            if hash_file(file, length, cancel)? != c.prefix_sha256 {
                return Err("Resume prefix digest mismatch".into());
            }
        }
    }
    cancelled(cancel)?;
    Ok(length)
}
fn range_size(headers: &header::HeaderMap, offset: u64, total: u64) -> Option<u64> {
    let values: Vec<_> = headers.get_all(header::CONTENT_RANGE).iter().collect();
    if values.len() != 1 {
        return None;
    }
    let value = values[0].to_str().ok()?.strip_prefix("bytes ")?;
    let (range, full) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, full) = (decimal(start)?, decimal(end)?, decimal(full)?);
    if start != offset || end < start || end >= total || full != total {
        return None;
    }
    let count = end - start + 1;
    declared_length(headers, count).ok()?;
    Some(count)
}
struct Events<F> {
    callback: F,
    operation: u64,
    component: usize,
    last: Option<Instant>,
    received: u64,
}
impl<F: FnMut(Progress)> Events<F> {
    fn emit(&mut self, phase: Phase, verified: u64, force: bool) {
        if force
            || self
                .last
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(100))
        {
            (self.callback)(Progress {
                operation: self.operation,
                component: self.component,
                phase,
                received: self.received,
                verified,
            });
            self.last = Some(Instant::now());
        }
    }
}
fn write_chunk(
    file: &mut File,
    chunk: &[u8],
    cancel: &AtomicBool,
    events: &mut Events<impl FnMut(Progress)>,
) -> Result<(), String> {
    #[cfg(test)]
    let mut file = tests::FaultWriter(file);
    // Each successful write advances a finite (at most 64-KiB) slice. Bound
    // Interrupted results cumulatively per chunk, including across partial writes.
    let mut remaining = chunk;
    let mut interrupted = 0_u8;
    while !remaining.is_empty() {
        cancelled(cancel)?;
        let count = match file.write(remaining) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                cancelled(cancel)?;
                interrupted += 1;
                if interrupted == 8 {
                    return Err(format!("Working-file write interrupted 8 times: {error}"));
                }
                continue;
            }
            Err(error) => return Err(io_error(error)),
        };
        if count == 0 {
            return Err(io_error(std::io::ErrorKind::WriteZero.into()));
        }
        events.received += count as u64;
        remaining = &remaining[count..];
        #[cfg(test)]
        tests::event("write", count as u64);
        cancelled(cancel)?;
        events.emit(Phase::Receiving, 0, false);
    }
    cancelled(cancel)
}
fn reset(
    file: &mut File,
    cancel: &AtomicBool,
    events: &mut Events<impl FnMut(Progress)>,
) -> Result<(), String> {
    cancelled(cancel)?;
    file.set_len(0).map_err(io_error)?;
    events.received = 0;
    #[cfg(test)]
    tests::event("reset", 0);
    cancelled(cancel)?;
    file.rewind().map_err(io_error)?;
    cancelled(cancel)?;
    events.emit(Phase::Resetting, 0, true);
    cancelled(cancel)
}
fn finish(
    component: ComponentView<'_>,
    file: &mut File,
    cancel: &AtomicBool,
    events: &mut Events<impl FnMut(Progress)>,
) -> Result<CompletedComponent, String> {
    cancelled(cancel)?;
    events.emit(Phase::Verifying, 0, true);
    cancelled(cancel)?;
    file.flush().map_err(io_error)?;
    cancelled(cancel)?;
    if hash_file(file, component.byte_size, cancel)? != component.sha256 {
        return Err("Complete component SHA-256 mismatch".into());
    }
    cancelled(cancel)?;
    events.received = component.byte_size;
    events.emit(Phase::Complete, component.byte_size, true);
    cancelled(cancel)?;
    Ok(CompletedComponent {
        byte_size: component.byte_size,
    })
}

#[cfg(test)]
mod tests;
