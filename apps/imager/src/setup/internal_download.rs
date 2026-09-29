//! Authenticated selection and bounded HTTP transfer through caller-owned handles.
//! This module neither owns cache paths nor publishes files. Authentication depends
//! on the embedded catalog belonging to a genuinely signed application.

use super::internal_bundle::{ComponentView, InternalBundle, MAX_MANIFEST_BYTES};
use super::internal_cache::{self as cache, PreparedPrefix};
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
    Url::parse("https://code.tilo.so/").expect("literal URL")
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
fn completion_stop(cancel: &AtomicBool, pause: Option<&AtomicBool>) -> Result<(), String> {
    cancelled(cancel)?;
    if pause.is_some_and(|p| p.load(Ordering::Acquire)) {
        Err("Internal transfer paused".into())
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
        let client = tilo_net::http::blocking_client()
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
        let mut response = self
            .request(url, 0, cancel, None)
            .map_err(|e| e.message())?;
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
        pause: Option<&AtomicBool>,
    ) -> Result<reqwest::blocking::Response, RequestError> {
        for hops in 0..=5 {
            cancelled(cancel).map_err(RequestError::Fatal)?;
            if pause.is_some_and(|p| p.load(Ordering::Acquire)) {
                return Err(RequestError::Paused);
            }
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
            // Stop before interpreting errors, redirects, status or framing.
            if pause.is_some_and(|p| p.load(Ordering::Acquire)) {
                return Err(RequestError::Paused);
            }
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
        let result =
            self.transfer_inner(selected, index, file, checkpoint, cancel, &mut events, None);
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
    /// Private provider seam. A successful result is still only invocation byte
    /// proof; native final publication and coordinator readiness remain separate.
    pub(super) fn transfer_persistent(
        &self,
        prepared: PreparedPrefix<'_>,
        cancel: &AtomicBool,
        pause: &AtomicBool,
        progress: impl FnMut(Progress),
    ) -> Result<PersistentOutcome, PersistentFailure> {
        let cache::TransferParts {
            selected,
            index,
            file,
            observer,
            id,
            hashes,
            committed,
            final_candidate,
        } = prepared.into_parts();
        let mut events = Events {
            callback: progress,
            operation: id.operation,
            component: index,
            last: None,
            received: hashes.length,
        };
        let mut durable = DurableTransfer {
            observer,
            id,
            hashes,
            committed,
            final_candidate,
            pause,
            paused: false,
            uncertain: false,
        };
        let result = self.transfer_inner(
            selected,
            index,
            file,
            None,
            cancel,
            &mut events,
            Some(&mut durable),
        );
        if cancel.load(Ordering::Acquire) {
            events.emit(Phase::Cancelled, 0, true);
            return Ok(PersistentOutcome::Cancelled {
                committed: durable.committed,
                error: result.err().filter(|e| e != "Internal transfer cancelled"),
                uncertain: durable.uncertain,
            });
        }
        if durable.paused {
            return Ok(PersistentOutcome::Paused {
                committed: durable.committed,
            });
        }
        match result {
            Ok(done) => Ok(PersistentOutcome::Complete(done)),
            Err(message) => {
                events.emit(Phase::Failed, 0, true);
                Err(PersistentFailure {
                    message,
                    committed: durable.committed,
                    uncertain: durable.uncertain,
                })
            }
        }
    }
    fn transfer_inner(
        &self,
        selected: &SelectedManifest,
        index: usize,
        file: &mut File,
        checkpoint: Option<&ResumeCheckpoint>,
        cancel: &AtomicBool,
        events: &mut Events<impl FnMut(Progress)>,
        mut durable: Option<&mut DurableTransfer<'_>>,
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
            let offset = if let Some(d) = durable.as_deref_mut() {
                d.id.attempt = attempt;
                d.observer.begin_attempt(d.id, file)?;
                if attempt > 0 {
                    d.hashes.retry(file, cancel)?;
                }
                if regular_length(file, component.byte_size, cancel)? != d.hashes.length {
                    return Err("Prepared prefix length changed".into());
                }
                d.hashes.length
            } else {
                validate_prefix(selected, component, file, checkpoint.as_deref(), cancel)?
            };
            if let Some(d) = durable.as_deref_mut() {
                if d.stop(file, component.byte_size, cancel)? {
                    return Err("Internal transfer paused".into());
                }
            }
            if offset == component.byte_size {
                return finish_persistent(component, file, cancel, events, durable.as_deref_mut());
            }
            cancelled(cancel)?;
            events.emit(Phase::Requesting, 0, true);
            cancelled(cancel)?;
            if let Some(d) = durable.as_deref_mut() {
                if d.stop(file, component.byte_size, cancel)? {
                    return Err("Internal transfer paused".into());
                }
            }
            let pause = durable.as_ref().map(|d| d.pause);
            let mut response = match self.request(url.clone(), offset, cancel, pause) {
                Err(RequestError::Paused) => {
                    if let Some(d) = durable.as_deref_mut() {
                        d.stop(file, component.byte_size, cancel)?;
                    }
                    return Err("Internal transfer paused".into());
                }
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
                if let Some(d) = durable.as_deref_mut() {
                    d.before_reset(file, component.byte_size, cancel)?;
                }
                reset(file, cancel, events)?;
                if let Some(d) = durable.as_deref_mut() {
                    d.hashes = cache::Hashes::empty();
                }
                checkpoint = None;
                continue;
            };
            cancelled(cancel)?;
            file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
            cancelled(cancel)?;
            events.emit(Phase::Receiving, 0, true);
            let pause = durable.as_ref().map(|d| d.pause);
            #[cfg(test)]
            let mut measured = tests::ResponseReader {
                response: &mut response,
                received: 0,
            };
            #[cfg(test)]
            let reader = &mut measured;
            #[cfg(not(test))]
            let reader = &mut response;
            let end = read_body_stoppable(
                reader,
                expected,
                cancel,
                || pause.is_some_and(|p| p.load(Ordering::Acquire)),
                |chunk| {
                    write_chunk(file, chunk, cancel, events)?;
                    if let Some(d) = durable.as_deref_mut() {
                        d.hashes.written(chunk);
                        if d.hashes.pending() >= checkpoint_interval()
                            && d.hashes.length < component.byte_size
                        {
                            d.commit(file, cancel, cache::Kind::Prefix)?;
                        }
                    }
                    Ok(())
                },
            )?;
            drop(response);
            let length = regular_length(file, component.byte_size, cancel)?;
            match end {
                BodyEnd::Complete if length == component.byte_size => {
                    return finish_persistent(
                        component,
                        file,
                        cancel,
                        events,
                        durable.as_deref_mut(),
                    );
                }
                BodyEnd::Paused => {
                    if let Some(d) = durable.as_deref_mut() {
                        d.stop(file, component.byte_size, cancel)?;
                    }
                    return Err("Internal transfer paused".into());
                }
                BodyEnd::Complete => {
                    last_error = "Partial range ended before complete component".into()
                }
                BodyEnd::Interrupted(error) => last_error = error,
            }
            // A full byte counter after a failed EOF is not a successful response.
            // Demand a fresh complete response within this invocation.
            if length == component.byte_size {
                if let Some(d) = durable.as_deref_mut() {
                    d.before_reset(file, component.byte_size, cancel)?;
                }
                reset(file, cancel, events)?;
                if let Some(d) = durable.as_deref_mut() {
                    d.hashes = cache::Hashes::empty();
                }
                checkpoint = None;
            } else if attempt < 2 {
                if let Some(d) = durable.as_deref_mut() {
                    if d.hashes.pending() > 0 {
                        d.commit(file, cancel, cache::Kind::Prefix)?;
                    }
                } else {
                    checkpoint = Some(Cow::Owned(capture_checkpoint(
                        selected, index, file, cancel,
                    )?));
                }
            }
            events.emit(Phase::Retrying, 0, true);
        }
        cancelled(cancel)?;
        if let Some(d) = durable.as_deref_mut() {
            if d.stop(file, component.byte_size, cancel)? {
                return Err("Internal transfer paused".into());
            }
        }
        Err(format!(
            "Component incomplete after 3 attempts ({} held bytes): {last_error}",
            regular_length(file, component.byte_size, cancel)?
        ))
    }
}

enum RequestError {
    Paused,
    Fatal(String),
    Retry(String),
}
impl RequestError {
    fn message(self) -> String {
        match self {
            Self::Fatal(s) | Self::Retry(s) => s,
            Self::Paused => "Internal transfer paused".into(),
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
    Paused,
    Interrupted(String),
}
/// Always consumes bounded EOF, including a sentinel read after the last expected byte.
fn read_body(
    reader: &mut impl Read,
    expected: u64,
    cancel: &AtomicBool,
    chunk: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<BodyEnd, String> {
    read_body_stoppable(reader, expected, cancel, || false, chunk)
}
fn read_body_stoppable(
    reader: &mut impl Read,
    expected: u64,
    cancel: &AtomicBool,
    pause: impl Fn() -> bool,
    mut chunk: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<BodyEnd, String> {
    let mut buffer = [0; BUFFER];
    let mut received = 0;
    loop {
        cancelled(cancel)?;
        if pause() {
            return Ok(BodyEnd::Paused);
        }
        let limit = ((expected - received + 1).min(BUFFER as u64)) as usize;
        let read = reader.read(&mut buffer[..limit]);
        cancelled(cancel)?;
        // A stop requested while this read was in flight precedes EOF/error.
        if pause() {
            return Ok(BodyEnd::Paused);
        }
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
    hash_file_stoppable(file, length, cancel, None)
}
fn hash_file_stoppable(
    file: &mut File,
    length: u64,
    cancel: &AtomicBool,
    pause: Option<&AtomicBool>,
) -> Result<String, String> {
    completion_stop(cancel, pause)?;
    if regular_length(file, length, cancel)? != length {
        return Err("Working file length changed".into());
    }
    file.rewind().map_err(io_error)?;
    completion_stop(cancel, pause)?;
    let mut hash = Sha256::new();
    #[cfg(test)]
    let mut count = 0;
    let end = {
        #[cfg(test)]
        let mut measured = tests::HashReader(file);
        #[cfg(test)]
        let reader = &mut measured;
        #[cfg(not(test))]
        let reader = &mut *file;
        read_body_stoppable(
            reader,
            length,
            cancel,
            || pause.is_some_and(|p| p.load(Ordering::Acquire)),
            |chunk| {
                hash.update(chunk);
                #[cfg(test)]
                {
                    count += chunk.len() as u64;
                    tests::event("hash", count);
                }
                completion_stop(cancel, pause)
            },
        )
    }?;
    match end {
        BodyEnd::Paused => return Err("Internal transfer paused".into()),
        BodyEnd::Interrupted(error) => return Err(error),
        BodyEnd::Complete => {}
    }
    #[cfg(test)]
    tests::event("hash-eof", count);
    completion_stop(cancel, pause)?;
    if regular_length(file, length, cancel)? != length {
        return Err("Working file length changed".into());
    }
    Ok(hex::encode(hash.finalize()))
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
            self.last = Some(ui::time::now());
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
    pause: Option<&AtomicBool>,
) -> Result<CompletedComponent, String> {
    completion_stop(cancel, pause)?;
    events.emit(Phase::Verifying, 0, true);
    completion_stop(cancel, pause)?;
    file.flush().map_err(io_error)?;
    completion_stop(cancel, pause)?;
    if hash_file_stoppable(file, component.byte_size, cancel, pause)? != component.sha256 {
        return Err("Complete component SHA-256 mismatch".into());
    }
    completion_stop(cancel, pause)?;
    events.received = component.byte_size;
    events.emit(Phase::Complete, component.byte_size, true);
    completion_stop(cancel, pause)?;
    Ok(CompletedComponent {
        byte_size: component.byte_size,
    })
}

#[cfg(test)]
pub(super) mod tests;

/// Last synchronously acknowledged prefix, never an inferred complete byte count.
#[derive(Debug)]
pub(super) enum PersistentOutcome {
    Complete(CompletedComponent),
    Paused {
        committed: u64,
    },
    Cancelled {
        committed: u64,
        error: Option<String>,
        uncertain: bool,
    },
}
#[derive(Debug)]
pub(super) struct PersistentFailure {
    pub message: String,
    pub committed: u64,
    pub uncertain: bool,
}
struct DurableTransfer<'a> {
    observer: &'a mut dyn cache::Observer,
    id: cache::BoundaryId,
    hashes: cache::Hashes,
    committed: u64,
    final_candidate: bool,
    pause: &'a AtomicBool,
    paused: bool,
    uncertain: bool,
}
impl DurableTransfer<'_> {
    fn acknowledge(&mut self, file: &mut File, candidate: cache::Candidate) -> Result<(), String> {
        let expected = cache::BoundaryId {
            generation: self
                .id
                .generation
                .checked_add(1)
                .ok_or("Checkpoint generation overflow")?,
            epoch: self
                .id
                .epoch
                .checked_add(u64::from(candidate.kind == cache::Kind::Reset))
                .ok_or("Checkpoint epoch overflow")?,
            ..self.id
        };
        self.uncertain = true;
        let ack = self.observer.boundary(self.id, file, candidate)?;
        if ack.id != expected {
            return Err("Stale durable checkpoint acknowledgement".into());
        }
        self.id = ack.id;
        self.uncertain = false;
        Ok(())
    }
    fn commit(
        &mut self,
        file: &mut File,
        cancel: &AtomicBool,
        kind: cache::Kind,
    ) -> Result<(), String> {
        if kind == cache::Kind::Final && self.stop(file, self.hashes.length, cancel)? {
            return Err("Internal transfer paused".into());
        }
        let verified = self.hashes.verify_tail(file, cancel);
        cancelled(cancel)?;
        if kind == cache::Kind::Final && self.stop(file, self.hashes.length, cancel)? {
            return Err("Internal transfer paused".into());
        }
        verified?;
        self.acknowledge(
            file,
            cache::Candidate {
                length: self.hashes.length,
                digest: self.hashes.digest(),
                kind,
            },
        )?;
        self.committed = self.hashes.length;
        cancelled(cancel)?;
        if kind == cache::Kind::Final && self.stop(file, self.hashes.length, cancel)? {
            return Err("Internal transfer paused".into());
        }
        Ok(())
    }
    fn before_reset(
        &mut self,
        file: &mut File,
        full: u64,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        if self.stop(file, full, cancel)? {
            return Err("Internal transfer paused".into());
        }
        self.acknowledge(
            file,
            cache::Candidate {
                length: 0,
                digest: Sha256::digest([]).into(),
                kind: cache::Kind::Reset,
            },
        )?;
        self.committed = 0;
        if self.stop(file, full, cancel)? {
            return Err("Internal transfer paused".into());
        }
        Ok(())
    }
    fn stop(&mut self, file: &mut File, full: u64, cancel: &AtomicBool) -> Result<bool, String> {
        cancelled(cancel)?;
        if !self.pause.load(Ordering::Acquire) {
            return Ok(false);
        }
        // Full bytes without successful response EOF must never become a candidate.
        if self.hashes.pending() > 0 && self.hashes.length < full {
            self.commit(file, cancel, cache::Kind::Prefix)?;
        }
        cancelled(cancel)?;
        self.paused = true;
        Ok(true)
    }
}
fn checkpoint_interval() -> u64 {
    #[cfg(test)]
    if let Some(value) = tests::checkpoint_interval_override() {
        return value;
    }
    cache::INTERVAL
}
fn finish_persistent(
    component: ComponentView<'_>,
    file: &mut File,
    cancel: &AtomicBool,
    events: &mut Events<impl FnMut(Progress)>,
    mut durable: Option<&mut DurableTransfer<'_>>,
) -> Result<CompletedComponent, String> {
    if let Some(d) = durable.as_deref_mut() {
        if d.stop(file, component.byte_size, cancel)? {
            return Err("Internal transfer paused".into());
        }
    }
    let pause = durable.as_ref().map(|d| d.pause);
    let result = finish(component, file, cancel, events, pause);
    if let Some(d) = durable.as_deref_mut() {
        if d.stop(file, component.byte_size, cancel)? {
            return Err("Internal transfer paused".into());
        }
    }
    let done = result?;
    if let Some(d) = durable {
        if !d.final_candidate {
            d.commit(file, cancel, cache::Kind::Final)?;
        }
    }
    cancelled(cancel)?;
    Ok(done)
}
