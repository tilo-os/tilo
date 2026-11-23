//! Authenticating a release and downloading it in bounded pieces.

use super::internal_bundle::{ComponentView, InternalBundle, MAX_MANIFEST_BYTES};
use super::internal_cache::{self as cache, PreparedPrefix};
use crate::release::InternalRelease;
use reqwest::Url;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use tilo_net::http::fetch;

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
fn asset_url(value: &str, origin: &Url) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|e| e.to_string())?;
    if !fetch::same_origin(&url, origin)
        || url
            .path_segments()
            .and_then(|s| s.last())
            .is_none_or(str::is_empty)
    {
        return Err("Unsupported selected asset URL".into());
    }
    Ok(url)
}
/// The error a durable step returns when the caller cancelled it, told apart
/// from a step that failed for a reason worth reporting.
const CANCELLED: &str = "Internal transfer cancelled";

fn cancelled(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err(CANCELLED.into())
    } else {
        Ok(())
    }
}
fn io_error(error: std::io::Error) -> String {
    format!("Internal working-file/stream error: {error}")
}

/// The internal release's own origin, and the verified transfer it runs.
pub struct InternalTransport {
    downloader: fetch::Downloader,
    origin: Url,
}
impl InternalTransport {
    pub fn new() -> Result<Self, String> {
        Self::client(production_origin(), Duration::from_secs(30))
    }
    fn client(origin: Url, timeout: Duration) -> Result<Self, String> {
        let downloader =
            fetch::Downloader::new(origin.clone(), timeout, None).map_err(|e| e.to_string())?;
        Ok(Self { downloader, origin })
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
        let bytes = self
            .downloader
            .fetch_bytes(fetch::Request {
                url,
                size: expected,
                sha256: Some(release.manifest_sha256()),
                cancel,
                pause: None,
            })
            .map_err(transfer_message)?;
        cancelled(cancel)?;
        SelectedManifest::checked(release, bytes, &self.origin)
    }
    /// `file` is the release's already-opened, exclusively owned working file
    /// for `index`. Bytes that arrive are recorded through the durable
    /// checkpoints this call drives, so a later operation resumes exactly what
    /// was proven.
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
        let component = match selected.component(index) {
            Ok(component) => component,
            Err(message) => {
                return Err(PersistentFailure {
                    message,
                    committed,
                    uncertain: false,
                });
            }
        };
        let url = selected.component_url(component);
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
        events.emit(Phase::Prefix, 0, true);
        let result = {
            let mut hooks = DurableHooks {
                durable: &mut durable,
                cancel,
                full: component.byte_size,
                attempt: 0,
            };
            self.downloader.fetch(
                fetch::Request {
                    url,
                    size: component.byte_size,
                    sha256: Some(component.sha256),
                    cancel,
                    pause: Some(pause),
                },
                &mut *file,
                &mut hooks,
                |update| events.report(update),
            )
        };
        let held = file.metadata().map_or(0, |metadata| metadata.len());
        if cancel.load(Ordering::Acquire) {
            events.emit(Phase::Cancelled, 0, true);
            return Ok(PersistentOutcome::Cancelled {
                committed: durable.committed,
                error: result
                    .err()
                    .filter(|error| !is_cancellation(error))
                    .map(|error| failure_message(error, held)),
                uncertain: durable.uncertain,
            });
        }
        if durable.paused {
            return Ok(PersistentOutcome::Paused {
                committed: durable.committed,
            });
        }
        match result {
            Ok(()) => Ok(PersistentOutcome::Complete(CompletedComponent {
                byte_size: component.byte_size,
            })),
            Err(error) => {
                events.emit(Phase::Failed, 0, true);
                Err(PersistentFailure {
                    message: failure_message(error, held),
                    committed: durable.committed,
                    uncertain: durable.uncertain,
                })
            }
        }
    }
}

fn transfer_message(error: fetch::Error) -> String {
    match error {
        fetch::Error::Cancelled => "Internal transfer cancelled".into(),
        fetch::Error::Paused => "Internal transfer paused".into(),
        fetch::Error::Trusted(detail) | fetch::Error::Protocol(detail) => detail,
        other => other.to_string(),
    }
}

fn is_cancellation(error: &fetch::Error) -> bool {
    match error {
        fetch::Error::Cancelled => true,
        fetch::Error::Hook(detail) => detail == CANCELLED,
        _ => false,
    }
}

fn failure_message(error: fetch::Error, held: u64) -> String {
    match error {
        fetch::Error::Digest => "Complete component SHA-256 mismatch".into(),
        fetch::Error::Incomplete(detail) => format!("Component incomplete {detail}, {held} held"),
        fetch::Error::Size { .. } => "Unexpected component byte size".into(),
        other => transfer_message(other),
    }
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
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub operation: u64,
    pub component: usize,
    pub phase: Phase,
    pub received: u64,
    pub verified: u64,
}
#[derive(Debug)]
pub struct CompletedComponent {
    byte_size: u64,
}
impl CompletedComponent {
    pub fn byte_size(&self) -> u64 {
        self.byte_size
    }
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
    fn report(&mut self, update: fetch::Progress) {
        self.received = update.received;
        let phase = match update.phase {
            fetch::Phase::Requesting => Phase::Requesting,
            fetch::Phase::Receiving => Phase::Receiving,
            fetch::Phase::Retrying => Phase::Retrying,
            fetch::Phase::Resetting => Phase::Resetting,
            fetch::Phase::Verifying => Phase::Verifying,
            fetch::Phase::Complete => Phase::Complete,
        };
        let verified = if phase == Phase::Complete {
            update.total
        } else {
            0
        };
        self.emit(phase, verified, phase != Phase::Receiving);
    }
}

#[cfg(test)]
pub(super) mod tests;

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
        if self.hashes.pending() > 0 && self.hashes.length < full {
            self.commit(file, cancel, cache::Kind::Prefix)?;
        }
        cancelled(cancel)?;
        self.paused = true;
        Ok(true)
    }
}

/// The durable side of a transfer: every chunk is written and recorded, and
/// every boundary the downloader reports settles what has been proven so far.
struct DurableHooks<'a, 'transfer> {
    durable: &'a mut DurableTransfer<'transfer>,
    cancel: &'a AtomicBool,
    full: u64,
    attempt: u8,
}
impl fetch::Hooks for DurableHooks<'_, '_> {
    fn attempt(&mut self, file: &mut File, _offset: u64) -> Result<(), String> {
        let durable = &mut *self.durable;
        durable.id.attempt = self.attempt;
        self.attempt = self
            .attempt
            .checked_add(1)
            .ok_or("Checkpoint attempt overflow")?;
        durable.observer.begin_attempt(durable.id, file)?;
        if durable.id.attempt > 0 {
            durable.hashes.retry(file, self.cancel)?;
        }
        if file.metadata().map_err(io_error)?.len() != durable.hashes.length {
            return Err("Prepared prefix length changed".into());
        }
        Ok(())
    }
    fn write(&mut self, file: &mut File, bytes: &[u8]) -> Result<(), String> {
        {
            #[cfg(test)]
            let mut faulted = tests::FaultWriter(&mut *file);
            #[cfg(test)]
            let target = &mut faulted;
            #[cfg(not(test))]
            let target = &mut *file;
            fetch::append(target, bytes)?;
        }
        #[cfg(test)]
        tests::event("write", bytes.len() as u64);
        let durable = &mut *self.durable;
        durable.hashes.written(bytes);
        if durable.hashes.pending() >= checkpoint_interval() && durable.hashes.length < self.full {
            durable.commit(file, self.cancel, cache::Kind::Prefix)?;
        }
        Ok(())
    }
    fn before_retry(&mut self, file: &mut File) -> Result<(), String> {
        let durable = &mut *self.durable;
        if durable.hashes.pending() > 0 {
            durable.commit(file, self.cancel, cache::Kind::Prefix)?;
        }
        Ok(())
    }
    fn before_reset(&mut self, file: &mut File) -> Result<(), String> {
        self.durable.before_reset(file, self.full, self.cancel)
    }
    fn reset(&mut self, _file: &mut File) -> Result<(), String> {
        self.durable.hashes = cache::Hashes::empty();
        Ok(())
    }
    fn pausing(&mut self, file: &mut File) -> Result<(), String> {
        self.durable.stop(file, self.full, self.cancel)?;
        Ok(())
    }
    fn verified(&mut self, file: &mut File) -> Result<(), String> {
        let durable = &mut *self.durable;
        if !durable.final_candidate {
            durable.commit(file, self.cancel, cache::Kind::Final)?;
        }
        Ok(())
    }
}

fn checkpoint_interval() -> u64 {
    #[cfg(test)]
    if let Some(value) = tests::checkpoint_interval_override() {
        return value;
    }
    cache::INTERVAL
}
