//! Independent, bounded cloud reads. No mount, change feed or upload worker is started.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cloud_source::materialize::{materialize_cancellable, preferred_export};
use cloud_source::{CloudSource, CredentialSource, Error, ItemId, ItemKind, Revision};
use runtime_fs::cloud::{
    CloudResponse, IMPORT_CHUNK_LIMIT, ImportEntry, ImportError, ImportErrorKind, ImportKind,
    ImportReadState, ImportReply, ImportRequest,
};

use super::providers::{self, MountSpec};

const SESSION_TTL: Duration = Duration::from_secs(300);
// Native exports are provider-bounded representations, not ordinary files.
const EXPORT_LIMIT: u64 = 32 << 20;
const MAX_SESSIONS: usize = 4;
const MAX_ACCOUNT_SESSIONS: usize = 2;
const MAX_JOBS: usize = 4;
const PAGE_SIZE: usize = 100;
const MAX_ENTRIES: usize = 20_000;

enum Content {
    Preparing,
    Ready {
        size: u64,
        requests: SyncSender<ChunkRequest>,
    },
    Failed(ImportError),
}
struct Session {
    account: String,
    expires: Instant,
    cancelled: Arc<AtomicBool>,
    account_cancelled: Arc<AtomicBool>,
    content: Content,
}
struct Account {
    spec: MountSpec,
    cancelled: Arc<AtomicBool>,
}
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// Disk staging is only for providers that omit their length. Known-length
/// ordinary files remain streamed, irrespective of these budgets.
struct Staging {
    directory: PathBuf,
    per_file: u64,
    aggregate: u64,
    used: Arc<AtomicU64>,
}
impl Staging {
    fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            per_file: 1 << 30,
            aggregate: 4 << 30,
            used: Arc::new(AtomicU64::new(0)),
        }
    }
}
struct DiskReservation {
    used: Arc<AtomicU64>,
    bytes: u64,
}
impl Drop for DiskReservation {
    fn drop(&mut self) {
        self.used.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

pub(super) struct Imports {
    clock: Clock,
    staging: Arc<Staging>,
    accounts: HashMap<String, Account>,
    sessions: Arc<Mutex<HashMap<String, Session>>>,
    jobs: Arc<AtomicUsize>,
}

pub(super) fn failure(kind: ImportErrorKind) -> ImportError {
    let message = match kind {
        ImportErrorKind::AuthenticationRequired => "Sign in to this account again.",
        ImportErrorKind::Offline => "The cloud service is unreachable.",
        ImportErrorKind::PermissionDenied => "Access to this account or item is denied.",
        ImportErrorKind::Unsupported => "This item has no supported file export.",
        ImportErrorKind::TooLarge => "This file exceeds the import size limit.",
        ImportErrorKind::SourceChanged => "The source changed. Browse and try again.",
        ImportErrorKind::Cancelled => "This read was cancelled or expired.",
        ImportErrorKind::QuotaExceeded => {
            "Too many cloud reads are active. Close one and try again."
        }
        ImportErrorKind::InvalidRequest => "The cloud read request is invalid.",
        ImportErrorKind::Other => "The cloud service could not complete this request.",
    };
    ImportError {
        kind,
        message: message.into(),
    }
}
fn provider_error(error: &Error) -> ImportError {
    failure(match error {
        Error::Unauthorized => ImportErrorKind::AuthenticationRequired,
        Error::Offline => ImportErrorKind::Offline,
        Error::Forbidden => ImportErrorKind::PermissionDenied,
        Error::Unsupported(_) => ImportErrorKind::Unsupported,
        Error::TooLarge => ImportErrorKind::TooLarge,
        Error::SessionExpired => ImportErrorKind::Cancelled,
        Error::Conflict { .. } | Error::NotFound | Error::CursorExpired => {
            ImportErrorKind::SourceChanged
        }
        _ => ImportErrorKind::Other,
    })
}
fn lease_deadline(clock: &Clock) -> Instant {
    let now = clock();
    now.checked_add(SESSION_TTL).unwrap_or(now)
}

fn response(reply: ImportReply, sender: &Sender<CloudResponse>) {
    let _ = sender.send(CloudResponse::Import(reply));
}

impl Imports {
    pub fn new(state_root: &Path) -> Self {
        Self {
            clock: Arc::new(Instant::now),
            staging: Arc::new(Staging::new(state_root.join("import-staging"))),
            accounts: HashMap::new(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            jobs: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Any removed, disabled, invalid or reconfigured account revokes its reads.
    pub fn reconcile(&mut self, specs: Vec<MountSpec>) {
        self.accounts.retain(|_, account| {
            let keep = specs.contains(&account.spec);
            if !keep {
                account.cancelled.store(true, Ordering::Release);
            }
            keep
        });
        for spec in specs {
            self.accounts
                .entry(spec.account_id.clone())
                .or_insert_with(|| Account {
                    spec,
                    cancelled: Arc::new(AtomicBool::new(false)),
                });
        }
        self.expire();
    }

    pub fn expire(&self) {
        let now = (self.clock)();
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, session| {
                let keep =
                    session.expires > now && !session.account_cancelled.load(Ordering::Acquire);
                if !keep {
                    session.cancelled.store(true, Ordering::Release);
                }
                keep
            });
    }

    pub fn next_expiry(&self) -> Option<Duration> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .map(|s| s.expires.saturating_duration_since((self.clock)()))
            .min()
    }

    pub fn cancel_all(&mut self) {
        self.reconcile(Vec::new());
    }

    fn start_read(
        &self,
        account: &Account,
        account_id: String,
        item: (String, Option<String>),
        credentials: Arc<dyn CredentialSource>,
        reply: &Sender<CloudResponse>,
    ) {
        let (id, revision) = item;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if sessions.len() >= MAX_SESSIONS
            || sessions
                .values()
                .filter(|s| s.account == account_id)
                .count()
                >= MAX_ACCOUNT_SESSIONS
            || self
                .jobs
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < MAX_JOBS).then_some(n.saturating_add(1))
                })
                .is_err()
        {
            response(
                ImportReply::Error(failure(ImportErrorKind::QuotaExceeded)),
                reply,
            );
            return;
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let cancelled = Arc::new(AtomicBool::new(false));
        let account_cancelled = account.cancelled.clone();
        sessions.insert(
            session_id.clone(),
            Session {
                account: account_id,
                expires: lease_deadline(&self.clock),
                cancelled: cancelled.clone(),
                account_cancelled: account_cancelled.clone(),
                content: Content::Preparing,
            },
        );
        drop(sessions);
        response(
            ImportReply::Preparing {
                session: session_id.clone(),
            },
            reply,
        );
        let spec = account.spec.clone();
        let sessions = self.sessions.clone();
        let jobs = self.jobs.clone();
        let clock = self.clock.clone();
        let staging = self.staging.clone();
        std::thread::spawn(move || {
            let revoked =
                || cancelled.load(Ordering::Acquire) || account_cancelled.load(Ordering::Acquire);
            let result = providers::open(&spec, credentials).and_then(|source| {
                Transfer::prepare(
                    source,
                    &ItemId::new(id),
                    revision.as_deref().map(Revision::new).as_ref(),
                    &revoked,
                    &staging,
                )
            });
            let (requests, incoming) = mpsc::sync_channel(1);
            {
                let mut sessions = sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(session) = sessions.get_mut(&session_id)
                    && !revoked()
                    && session.expires > clock()
                {
                    session.content = match &result {
                        Ok(transfer) => Content::Ready {
                            size: transfer.size,
                            requests,
                        },
                        Err(e) => Content::Failed(provider_error(e)),
                    };
                }
            }
            if let Ok(mut transfer) = result {
                run_chunks(
                    &mut transfer,
                    &incoming,
                    &sessions,
                    &session_id,
                    &revoked,
                    &clock,
                );
            }
            jobs.fetch_sub(1, Ordering::AcqRel);
        });
    }

    pub fn request(
        &self,
        account_id: String,
        request: ImportRequest,
        credentials: Arc<dyn CredentialSource>,
        reply: Sender<CloudResponse>,
    ) {
        self.expire();
        let Some(account) = self.accounts.get(&account_id) else {
            response(
                ImportReply::Error(failure(ImportErrorKind::PermissionDenied)),
                &reply,
            );
            return;
        };
        match request {
            ImportRequest::Browse { folder, offset } => {
                if self
                    .jobs
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                        (n < MAX_JOBS).then_some(n.saturating_add(1))
                    })
                    .is_err()
                {
                    response(
                        ImportReply::Error(failure(ImportErrorKind::QuotaExceeded)),
                        &reply,
                    );
                    return;
                }
                let spec = account.spec.clone();
                let cancelled = account.cancelled.clone();
                let jobs = self.jobs.clone();
                std::thread::spawn(move || {
                    let result = providers::open(&spec, credentials)
                        .and_then(|source| browse(source.as_ref(), folder, offset));
                    let answer = if cancelled.load(Ordering::Acquire) {
                        ImportReply::Error(failure(ImportErrorKind::Cancelled))
                    } else {
                        result.unwrap_or_else(|e| ImportReply::Error(provider_error(&e)))
                    };
                    response(answer, &reply);
                    jobs.fetch_sub(1, Ordering::AcqRel);
                });
            }
            ImportRequest::StartRead { id, revision } => {
                self.start_read(account, account_id, (id, revision), credentials, &reply);
            }
            request => self.session_request(&account_id, request, &reply),
        }
    }
    fn session_request(
        &self,
        account_id: &str,
        request: ImportRequest,
        reply: &Sender<CloudResponse>,
    ) {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (ImportRequest::ReadStatus { session: id }
        | ImportRequest::ReadChunk { session: id, .. }
        | ImportRequest::CloseRead { session: id }) = &request
        else {
            return;
        };
        let valid = sessions.get(id).is_some_and(|s| s.account == account_id);
        let answer = if matches!(request, ImportRequest::CloseRead { .. }) {
            if valid && let Some(session) = sessions.remove(id) {
                session.cancelled.store(true, Ordering::Release);
            }
            ImportReply::Closed
        } else if !valid {
            ImportReply::Error(failure(ImportErrorKind::Cancelled))
        } else if let Some(session) = sessions.get_mut(id) {
            // Only authenticated, valid activity renews an idle lease.
            // Chunk offsets are validated by the sequential worker.
            if matches!(request, ImportRequest::ReadStatus { .. }) {
                session.expires = lease_deadline(&self.clock);
            }
            match (&session.content, request) {
                (Content::Preparing, ImportRequest::ReadStatus { .. }) => {
                    ImportReply::ReadState(ImportReadState::Preparing)
                }
                (Content::Failed(error), _) => {
                    ImportReply::ReadState(ImportReadState::Failed(error.clone()))
                }
                (Content::Ready { size, .. }, ImportRequest::ReadStatus { .. }) => {
                    ImportReply::ReadState(ImportReadState::Ready { size: *size })
                }
                (
                    Content::Ready { requests, .. },
                    ImportRequest::ReadChunk { offset, length, .. },
                ) => {
                    let requests = requests.clone();
                    drop(sessions);
                    if requests
                        .try_send(ChunkRequest {
                            offset,
                            length,
                            reply: reply.clone(),
                        })
                        .is_err()
                    {
                        response(
                            ImportReply::Error(failure(ImportErrorKind::QuotaExceeded)),
                            reply,
                        );
                    }
                    return;
                }
                _ => ImportReply::Error(failure(ImportErrorKind::InvalidRequest)),
            }
        } else {
            ImportReply::Error(failure(ImportErrorKind::Cancelled))
        };
        response(answer, reply);
    }
}

fn run_chunks(
    transfer: &mut Transfer,
    incoming: &mpsc::Receiver<ChunkRequest>,
    sessions: &Mutex<HashMap<String, Session>>,
    session_id: &str,
    revoked: &impl Fn() -> bool,
    clock: &Clock,
) {
    while !revoked() {
        let request = match incoming.recv_timeout(Duration::from_millis(100)) {
            Ok(request) => request,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if request.offset != transfer.offset
            || request.length == 0
            || request.length > IMPORT_CHUNK_LIMIT
        {
            response(
                ImportReply::Error(failure(ImportErrorKind::InvalidRequest)),
                &request.reply,
            );
            continue;
        }
        {
            let mut sessions = sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(session) = sessions.get_mut(session_id) {
                session.expires = lease_deadline(clock);
            }
        }
        let answer = transfer.chunk(request.offset, request.length, revoked);
        let answer = if revoked() {
            Err(Error::SessionExpired)
        } else {
            answer
        };
        let failed = answer.as_ref().err().map(provider_error);
        response(
            answer.unwrap_or_else(|e| ImportReply::Error(provider_error(&e))),
            &request.reply,
        );
        if let Some(error) = failed {
            // Invalid offsets do not destroy a valid sequential stream.
            if error.kind != ImportErrorKind::InvalidRequest {
                let mut sessions = sessions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(session) = sessions.get_mut(session_id) {
                    session.content = Content::Failed(error);
                }
                break;
            }
        }
    }
    // Account revocation may leave a second request queued behind body IO.
    // Release its waiter with a typed cancellation instead of silently dropping it.
    for request in incoming.try_iter() {
        response(
            ImportReply::Error(failure(ImportErrorKind::Cancelled)),
            &request.reply,
        );
    }
}

struct ChunkRequest {
    offset: u64,
    length: u32,
    reply: Sender<CloudResponse>,
}

/// Owned only by a read worker. Ordinary known-length bodies are never cached
/// or materialized; providers without a length use quota-accounted disk
/// staging in the service state directory, never the system temporary directory.
struct Transfer {
    body: Box<dyn Read + Send>,
    source: Arc<dyn CloudSource>,
    before: cloud_source::Item,
    size: u64,
    offset: u64,
    // Drop the file descriptor before returning its disk reservation.
    _disk: Option<DiskReservation>,
}

impl Transfer {
    fn prepare(
        source: Arc<dyn CloudSource>,
        id: &ItemId,
        revision: Option<&Revision>,
        cancelled: &impl Fn() -> bool,
        staging: &Staging,
    ) -> cloud_source::Result<Self> {
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let before = source.stat(id)?;
        if revision.is_some() && before.revision.as_ref() != revision {
            return Err(Error::Conflict {
                current: before.revision,
            });
        }
        let mut disk = None;
        let (body, size): (Box<dyn Read + Send>, u64) = match before.kind {
            ItemKind::Native(_) => {
                let bytes = materialize_cancellable(
                    source.as_ref(),
                    id,
                    before.revision.as_ref(),
                    EXPORT_LIMIT,
                    cancelled,
                )?;
                let size = bytes.len() as u64;
                (Box::new(std::io::Cursor::new(bytes)), size)
            }
            ItemKind::File => {
                if cancelled() {
                    return Err(Error::SessionExpired);
                }
                let download = source.read(id, None, before.revision.as_ref())?;
                if download.total.zip(before.size).is_some_and(|(a, b)| a != b) {
                    return Err(Error::InvalidResponse("download size"));
                }
                if let Some(size) = download.total.or(before.size) {
                    (download.body, size)
                } else {
                    let mut body = download.body;
                    std::fs::create_dir_all(&staging.directory)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            &staging.directory,
                            std::fs::Permissions::from_mode(0o700),
                        )?;
                    }
                    let mut file = tempfile::tempfile_in(&staging.directory)?;
                    let mut reservation = DiskReservation {
                        used: staging.used.clone(),
                        bytes: 0,
                    };
                    let mut buffer = [0_u8; 16 * 1024];
                    let mut size = 0_u64;
                    loop {
                        if cancelled() {
                            return Err(Error::SessionExpired);
                        }
                        let count = body.read(&mut buffer)?;
                        if count == 0 {
                            break;
                        }
                        let next = size.checked_add(count as u64).ok_or(Error::TooLarge)?;
                        if next > staging.per_file {
                            return Err(Error::TooLarge);
                        }
                        staging
                            .used
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                                used.checked_add(count as u64)
                                    .filter(|next| *next <= staging.aggregate)
                            })
                            .map_err(|_| Error::TooLarge)?;
                        reservation.bytes = next;
                        file.write_all(
                            buffer
                                .get(..count)
                                .ok_or(Error::InvalidResponse("read length"))?,
                        )?;
                        size = size.checked_add(count as u64).ok_or(Error::TooLarge)?;
                    }
                    Self::validate(source.as_ref(), &before, cancelled)?;
                    file.seek(SeekFrom::Start(0))?;
                    disk = Some(reservation);
                    (Box::new(file), size)
                }
            }
            ItemKind::Folder => return Err(Error::Unsupported("reading folders")),
        };
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let mut transfer = Self {
            _disk: disk,
            body,
            source,
            before,
            size,
            offset: 0,
        };
        // An empty transfer has no client chunk requests; verify it before
        // publishing Ready so clients cannot finalize an unchecked EOF.
        if size == 0 {
            transfer.chunk(0, 1, cancelled)?;
        }
        Ok(transfer)
    }

    fn validate(
        source: &dyn CloudSource,
        before: &cloud_source::Item,
        cancelled: &impl Fn() -> bool,
    ) -> cloud_source::Result<()> {
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        let after = source.stat(&before.id)?;
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        if before.revision != after.revision
            || before.kind != after.kind
            || before.size != after.size
        {
            return Err(Error::Conflict {
                current: after.revision,
            });
        }
        Ok(())
    }

    fn chunk(
        &mut self,
        offset: u64,
        length: u32,
        cancelled: &impl Fn() -> bool,
    ) -> cloud_source::Result<ImportReply> {
        let remaining = self
            .size
            .checked_sub(offset)
            .ok_or(Error::InvalidResponse("read offset"))?;
        let count =
            usize::try_from(remaining.min(u64::from(length))).map_err(|_| Error::TooLarge)?;
        let mut bytes = vec![0; count];
        let mut filled = 0;
        while filled < count {
            if cancelled() {
                return Err(Error::SessionExpired);
            }
            let n = self.body.read(
                bytes
                    .get_mut(filled..)
                    .ok_or(Error::InvalidResponse("read length"))?,
            )?;
            if n == 0 {
                return Err(Error::InvalidResponse("incomplete download"));
            }
            filled = filled.saturating_add(n);
        }
        let end = offset.saturating_add(count as u64);
        let eof = end == self.size;
        if eof {
            // EOF is not inferred solely from Content-Length. Do not release
            // the final chunk until the body ended and metadata was rechecked.
            if cancelled() {
                return Err(Error::SessionExpired);
            }
            if self.body.read(&mut [0_u8; 1])? != 0 {
                return Err(Error::InvalidResponse("excess download body"));
            }
            Self::validate(self.source.as_ref(), &self.before, cancelled)?;
        }
        if cancelled() {
            return Err(Error::SessionExpired);
        }
        self.offset = end;
        Ok(ImportReply::Chunk { bytes, eof })
    }
}

fn browse(
    source: &dyn CloudSource,
    folder: Option<String>,
    offset: u32,
) -> cloud_source::Result<ImportReply> {
    let folder = folder.map_or_else(|| source.root(), ItemId::new);
    if folder != source.root() && !source.stat(&folder)?.is_folder() {
        return Err(Error::NotFound);
    }
    let items = source.list(&folder)?;
    if items.len() > MAX_ENTRIES {
        return Err(Error::TooLarge);
    }
    let mut tree = cloud_source::tree::Tree::new(source.root());
    for item in items {
        tree.upsert(item);
    }
    let mut entries: Vec<_> = tree
        .children(&folder)
        .into_iter()
        .map(|(name, item)| {
            let kind = match &item.kind {
                ItemKind::Folder => ImportKind::Folder,
                ItemKind::File => ImportKind::File,
                ItemKind::Native(native) if preferred_export(native).is_ok() => {
                    ImportKind::NativeExport
                }
                ItemKind::Native(_) => ImportKind::Unsupported,
            };
            ImportEntry {
                id: item.id.0.clone(),
                name,
                kind,
                revision: item.revision.as_ref().map(|r| r.0.clone()),
                size: if matches!(item.kind, ItemKind::Native(_)) {
                    None
                } else {
                    item.size
                },
                content_writable: item.kind == ItemKind::File,
            }
        })
        .collect();
    // IDs, not provider result order or mutable titles, define pagination order.
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    let offset = offset as usize;
    if offset > entries.len() {
        return Err(Error::CursorExpired);
    }
    let end = offset.saturating_add(PAGE_SIZE).min(entries.len());
    Ok(ImportReply::Page {
        folder: folder.0,
        next_offset: (end < entries.len()).then(|| u32::try_from(end).unwrap_or(u32::MAX)),
        entries: entries.into_iter().skip(offset).take(PAGE_SIZE).collect(),
    })
}

#[cfg(test)]
#[path = "import_tests.rs"]
mod streaming_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_and_account_revocation_cancel_workers_without_revoking_other_reads() {
        let dir = tempfile::tempdir().unwrap();
        let mut imports = Imports::new(dir.path());
        let spec = MountSpec {
            account_id: "account".into(),
            provider: "webdav".into(),
            config: serde_json::Value::Null,
            cache_budget: 0,
            paused: false,
        };
        imports.reconcile(vec![spec]);
        assert!(imports.accounts.contains_key("account"));
        let Some(account) = imports.accounts.get("account") else {
            return;
        };
        let account_cancelled = account.cancelled.clone();
        let expired = Arc::new(AtomicBool::new(false));
        let active = Arc::new(AtomicBool::new(false));
        {
            let mut sessions = imports
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            for (id, expires, cancelled) in [
                ("expired", Instant::now(), expired.clone()),
                (
                    "active",
                    Instant::now()
                        .checked_add(SESSION_TTL)
                        .unwrap_or_else(Instant::now),
                    active.clone(),
                ),
            ] {
                sessions.insert(
                    id.into(),
                    Session {
                        account: "account".into(),
                        expires,
                        cancelled,
                        account_cancelled: account_cancelled.clone(),
                        content: Content::Preparing,
                    },
                );
            }
        }
        imports.expire();
        assert!(expired.load(Ordering::Acquire));
        assert!(!active.load(Ordering::Acquire));
        assert!(!account_cancelled.load(Ordering::Acquire));
        assert_eq!(
            imports
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        imports.reconcile(Vec::new());
        assert!(active.load(Ordering::Acquire));
        assert!(account_cancelled.load(Ordering::Acquire));
        assert!(
            imports
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );
    }
}
