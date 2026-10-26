//! The door's outbox: a mail is kept until a mail service takes it, so a
//! restart, a failing service or a busy one delays a confirmation, an
//! invitation or word of a removal instead of losing it.
//!
//! In the persistent room the outbox is durable: one file per mail in the
//! room's own state directory, created `0600` in a `0700` directory and
//! published atomically, so a crash leaves either the whole entry or none.
//! An entry holds the mail itself — including its links, until it is sent or
//! ages out — and never the mail service's key. It is bounded in number,
//! bytes and age, and nothing polls: the sender waits on a notification or
//! on the next attempt's time.
//!
//! Delivery is at least once. An attempt is written down before the service
//! is asked, so a crash mid-attempt does not retry forever; an entry whose
//! file cannot be removed after a successful send is sent again after a
//! restart, and every attempt carries the entry's id as the service's
//! idempotency key, so that repeated attempt is one mail.

use futures::future::BoxFuture;
use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tokio::sync::Notify;

use crate::mail::{Kind, Mail, Mailer, SendError};

/// The format of one outbox file.
const VERSION: u32 = 1;

/// The most an outbox file may be: a mail, not a document.
const MAX_ENTRY_BYTES: u64 = 1024 * 1024;

/// A mail older than this is not sent: a confirmation link that old is
/// stale, and a door that was down for a day must not deliver a burst.
const MAX_AGE_MS: u64 = 24 * 60 * 60 * 1000;

/// The most mails the outbox keeps.
const MAX_ITEMS: usize = 512;

/// The most bytes the outbox keeps.
const MAX_BYTES: usize = 16 * 1024 * 1024;

/// The waits before each further attempt: the first is prompt, the rest back
/// off. A mail that fails once more than this many times is given up.
static RETRY_MS: [u64; 7] = [
    5_000, 30_000, 120_000, 600_000, 1_800_000, 3_600_000, 7_200_000,
];

/// The longest an attempt is put off, even when the service asks for more.
const MAX_WAIT_MS: u64 = 3_600_000;

/// How long an idle outbox sleeps before it looks again, if only for entries
/// that have aged out.
const IDLE_MS: u64 = 60_000;

/// How the outbox is bounded and when it tries again.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_items: usize,
    pub max_bytes: usize,
    pub max_age_ms: u64,
    pub retry_ms: &'static [u64],
    pub max_wait_ms: u64,
}

impl Limits {
    pub(crate) const DEFAULT: Self = Self {
        max_items: MAX_ITEMS,
        max_bytes: MAX_BYTES,
        max_age_ms: MAX_AGE_MS,
        retry_ms: &RETRY_MS,
        max_wait_ms: MAX_WAIT_MS,
    };
}

/// One queued mail, as the outbox keeps it.
#[derive(Clone)]
struct Item {
    id: String,
    kind: Kind,
    created_ms: u64,
    /// Attempts begun so far.
    attempts: u32,
    /// When the next attempt may begin.
    next_at_ms: u64,
    /// How much room this entry takes.
    bytes: usize,
    /// Where it is kept, when the outbox is durable.
    path: Option<PathBuf>,
    mail: Mail,
    removal: Option<String>,
}

#[derive(Default)]
struct State {
    items: Vec<Item>,
    bytes: usize,
    receipts: BTreeMap<String, u64>,
    cancellation_revision: u64,
}

pub(crate) type Eligibility = Arc<dyn Fn(String) -> BoxFuture<'static, Option<bool>> + Send + Sync>;

/// What one outbox file holds.
#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    id: String,
    kind: Kind,
    created_ms: u64,
    attempts: u32,
    next_at_ms: u64,
    mail: Mail,
    #[serde(default)]
    removal: Option<String>,
}

/// One mail being attempted now.
struct Taken {
    id: String,
    kind: Kind,
    mail: Mail,
}

pub(crate) struct Outbox {
    /// Where entries are kept between starts; `None`, only while it runs.
    dir: Option<PathBuf>,
    limits: Limits,
    mailer: Arc<dyn Mailer>,
    state: Mutex<State>,
    eligibility: Mutex<Option<Eligibility>>,
    /// A mail was queued: the sender need not wait out its sleep.
    wake: Notify,
    /// A sender task runs.
    worker: AtomicBool,
    /// One mail is with the service right now.
    sending: AtomicBool,
}

impl Outbox {
    /// An outbox that keeps its mail only while the door runs.
    pub(crate) fn in_memory(mailer: Arc<dyn Mailer>, limits: Limits) -> Self {
        Self {
            dir: None,
            limits,
            mailer,
            state: Mutex::default(),
            eligibility: Mutex::default(),
            wake: Notify::new(),
            worker: AtomicBool::new(false),
            sending: AtomicBool::new(false),
        }
    }

    /// An outbox in `dir`, keeping what a previous run did not send. The
    /// directory and its files are private to this user; a directory that
    /// cannot be prepared is an error, never an outbox that forgets.
    pub(crate) fn durable(dir: &Path, mailer: Arc<dyn Mailer>, limits: Limits) -> Result<Self> {
        let absolute = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            std::env::current_dir()?.join(dir)
        };
        let dir = absolute.as_path();
        tilo_files::native::private::directory(dir)
            .with_context(|| format!("create {}", dir.display()))?;
        // Flush every ancestor: private::directory may have created more
        // than the leaf, and syncing an entry does not publish its parent.
        for parent in dir
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
        {
            tilo_files::native::private::sync_directory(parent)?;
        }
        let outbox = Self {
            dir: Some(dir.to_path_buf()),
            ..Self::in_memory(mailer, limits)
        };
        outbox.restore()?;
        let receipts = dir.join("receipts.json");
        if receipts.exists() {
            let (mut file, _) =
                tilo_files::native::private::open_owned_for_read(&receipts, MAX_ENTRY_BYTES)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            outbox.state.lock_recover().receipts = serde_json::from_slice(&bytes)?;
        }
        Ok(outbox)
    }

    pub(crate) fn set_eligibility(&self, check: Eligibility) {
        *self.eligibility.lock_recover() = Some(check);
        self.wake.notify_one();
    }

    /// The relay suppresses repeat confirmations for an hour. Only durable
    /// acceptance by this outbox is evidence that suppression is successful.
    pub(crate) fn confirmation_kept(&self, contact: &str) -> bool {
        let key = receipt_key(contact);
        self.state
            .lock_recover()
            .receipts
            .get(&key)
            .is_some_and(|at| tilo_clock::unix_now_ms().saturating_sub(*at) < 3_600_000)
    }

    pub(crate) fn confirmation_receipt(&self, contact: &str, accepted: bool) -> bool {
        let now = tilo_clock::unix_now_ms();
        let mut state = self.state.lock_recover();
        let mut receipts = state.receipts.clone();
        receipts.retain(|_, at| now.saturating_sub(*at) < 3_600_000);
        let key = receipt_key(contact);
        receipts.remove(&key);
        if accepted {
            while receipts.len() >= MAX_ITEMS {
                let oldest = receipts
                    .iter()
                    .min_by_key(|(_, at)| *at)
                    .map(|(key, _)| key.clone());
                if let Some(oldest) = oldest {
                    receipts.remove(&oldest);
                } else {
                    break;
                }
            }
            receipts.insert(key, now);
        }
        if receipts == state.receipts {
            return true;
        }
        if !self.save_receipts(&receipts) {
            return false;
        }
        state.receipts = receipts;
        true
    }

    fn save_receipts(&self, receipts: &BTreeMap<String, u64>) -> bool {
        if let Some(dir) = &self.dir {
            let result = serde_json::to_vec(receipts)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    tilo_files::native::private::write_atomic(&dir.join("receipts.json"), &bytes)
                });
            if result.is_err() {
                return false;
            }
        }
        true
    }

    /// The relay's durable removal is authoritative even if a file cannot
    /// be deleted. The eligibility check also covers other removal tokens
    /// for the same entry and restarts during cancellation.
    pub(crate) fn cancel(&self, removal: &str, contact_sha256: Option<&str>) {
        // Another valid removal token can name the same unconfirmed
        // recipient without disclosing its address. Invalidate in-flight
        // eligibility snapshots even when no local token matches.
        let mut state = self.state.lock_recover();
        state.cancellation_revision = state.cancellation_revision.wrapping_add(1);
        let mut kept = Vec::new();
        let mut cancelled_contacts = Vec::new();
        for item in std::mem::take(&mut state.items) {
            if item.kind != Kind::Removed
                && (item.removal.as_deref() == Some(removal)
                    || contact_sha256.is_some_and(|key| receipt_key(&item.mail.to) == key))
            {
                cancelled_contacts.push(item.mail.to.clone());
                discard_file(&item);
            } else {
                kept.push(item);
            }
        }
        state.bytes = kept.iter().map(|item| item.bytes).sum();
        state.items = kept;
        let mut receipts_changed = false;
        if let Some(key) = contact_sha256 {
            receipts_changed |= state.receipts.remove(key).is_some();
        }
        for contact in cancelled_contacts {
            receipts_changed |= state.receipts.remove(&receipt_key(&contact)).is_some();
        }
        if receipts_changed && !self.save_receipts(&state.receipts) {
            eprintln!("tilo-room-service: could not clear a cancelled confirmation receipt");
        }
        drop(state);
        if let Some(dir) = &self.dir
            && tilo_files::native::private::sync_directory(dir).is_err()
        {
            eprintln!(
                "tilo-room-service: cancellation files could not be flushed; eligibility remains authoritative"
            );
        }
    }

    /// Start the sender task, if a runtime is there to run it. Idempotent:
    /// the first caller starts it, and it is started once however many
    /// callers ask.
    pub(crate) fn start(self: &Arc<Self>) {
        if self.worker.swap(true, Ordering::AcqRel) {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.worker.store(false, Ordering::Release);
            return;
        };
        handle.spawn(Arc::clone(self).run());
    }

    /// Read what a previous run left, dropping entries it can no longer
    /// send and the oldest beyond the bounds.
    fn restore(&self) -> Result<()> {
        let Some(dir) = &self.dir else {
            return Ok(());
        };
        let now = tilo_clock::unix_now_ms();
        let mut items = Vec::new();
        let mut unreadable = 0_usize;
        for entry in std::fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
            let entry = entry.with_context(|| format!("read {}", dir.display()))?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
                unreadable = unreadable.saturating_add(1);
                discard_path(&path);
                continue;
            };
            if !is_entry_name(name) {
                continue;
            }
            match read_entry(&path) {
                Ok(item) if !aged_out(&item, self.limits, now) => items.push(item),
                // An entry nobody can read, or one too old to send, is
                // dropped: keeping it would only fail again.
                Ok(item) => discard_file(&item),
                Err(_) => {
                    unreadable = unreadable.saturating_add(1);
                    discard_path(&path);
                }
            }
        }
        if unreadable > 0 {
            eprintln!("tilo-room-service: set aside {unreadable} unreadable mail outbox file(s)");
        }
        items.sort_by_key(|item| item.created_ms);
        let dropped = {
            let mut state = self.state.lock_recover();
            for item in std::mem::take(&mut items) {
                state.bytes = state.bytes.saturating_add(item.bytes);
                state.items.push(item);
            }
            let mut dropped = Vec::new();
            while state.items.len() > self.limits.max_items || state.bytes > self.limits.max_bytes {
                let item = state.items.remove(0);
                state.bytes = state.bytes.saturating_sub(item.bytes);
                dropped.push(item);
            }
            drop(state);
            dropped
        };
        if !dropped.is_empty() {
            eprintln!(
                "tilo-room-service: dropped {} queued mail(s) beyond what the outbox keeps",
                dropped.len()
            );
            for item in &dropped {
                discard_file(item);
            }
        }
        Ok(())
    }

    /// Keep `mail` until it is sent: whether the outbox has room for it.
    pub(crate) fn enqueue(&self, kind: Kind, mail: Mail) -> bool {
        self.enqueue_for(kind, mail, None)
    }

    pub(crate) fn enqueue_for(&self, kind: Kind, mail: Mail, removal: Option<String>) -> bool {
        if !mail.is_sendable() {
            eprintln!("tilo-room-service: {kind} was not queued: the mail is not well formed");
            return false;
        }
        if u64::try_from(mail.weight()).unwrap_or(u64::MAX) > MAX_ENTRY_BYTES {
            eprintln!("tilo-room-service: {kind} was not queued: the mail is too large");
            return false;
        }
        let now = tilo_clock::unix_now_ms();
        let id = hex::encode(rand::random::<[u8; 16]>());
        let item = Item {
            path: self
                .dir
                .as_ref()
                .map(|dir| dir.join(format!("{now:013}-{id}.json"))),
            id,
            kind,
            created_ms: now,
            attempts: 0,
            next_at_ms: now,
            bytes: mail.weight(),
            mail,
            removal,
        };
        let mut state = self.state.lock_recover();
        let room = state.items.len() < self.limits.max_items
            && state.bytes.saturating_add(item.bytes) <= self.limits.max_bytes;
        if !room {
            drop(state);
            discard_file(&item);
            eprintln!("tilo-room-service: the door's outbox is full, so {kind} was not queued");
            return false;
        }
        if let Err(error) = save(&item) {
            eprintln!(
                "tilo-room-service: {kind} was not queued: {}",
                reason(&error)
            );
            return false;
        }
        state.bytes = state.bytes.saturating_add(item.bytes);
        state.items.push(item);
        drop(state);
        self.wake.notify_one();
        true
    }

    /// Send, or make way for, at most one due mail; whether it did
    /// anything, so the sender knows to look again at once.
    async fn deliver_due(&self, now_ms: u64) -> bool {
        if self
            .sending
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        // Eligibility is not a provider attempt: a relay restart must not
        // consume the finite send budget while its durable state is offline.
        let (candidate, revision) = {
            let state = self.state.lock_recover();
            let candidate = state
                .items
                .iter()
                .find(|item| {
                    item.next_at_ms <= now_ms
                        && !aged_out(item, self.limits, now_ms)
                        && usize::try_from(item.attempts).unwrap_or(usize::MAX)
                            <= self.limits.retry_ms.len()
                })
                .cloned();
            (candidate, state.cancellation_revision)
        };
        let candidate_id = candidate.as_ref().map(|item| item.id.clone());
        if let Some(item) = &candidate
            && item.kind != Kind::Removed
            && item.removal.is_none()
            && self.eligibility.lock_recover().is_some()
        {
            // Recipient-dependent mail without eligibility metadata is
            // not safe to send from a persistent room.
            self.finish(&item.id);
            self.sending.store(false, Ordering::Release);
            return true;
        }
        if let Some(item) = candidate.filter(|item| item.removal.is_some()) {
            let check = self.eligibility.lock_recover().clone();
            let eligible = match check {
                Some(check) => check(item.removal.clone().unwrap_or_default()).await,
                None => None,
            };
            if eligible != Some(true) {
                if eligible == Some(false) {
                    if item.kind == Kind::Confirm {
                        self.confirmation_receipt(&item.mail.to, false);
                    }
                    self.finish(&item.id);
                } else {
                    let mut state = self.state.lock_recover();
                    if let Some(item) = state.items.iter_mut().find(|entry| entry.id == item.id) {
                        item.next_at_ms = now_ms.saturating_add(30_000);
                        if save(item).is_err() {
                            eprintln!("tilo-room-service: could not persist eligibility backoff");
                        }
                    }
                }
                self.sending.store(false, Ordering::Release);
                return true;
            }
        }
        let Some(Taken { id, kind, mail }) = self.take_due_for(
            now_ms,
            candidate_id.as_deref().unwrap_or_default(),
            revision,
        ) else {
            self.sending.store(false, Ordering::Release);
            return self.state.lock_recover().cancellation_revision != revision;
        };
        let result = self.mailer.send(&id, mail).await;
        self.settle(tilo_clock::unix_now_ms(), &id, kind, result);
        self.sending.store(false, Ordering::Release);
        true
    }

    /// Wait for a queued mail, or for the next attempt's time.
    async fn run(self: Arc<Self>) {
        loop {
            let now = tilo_clock::unix_now_ms();
            if self.deliver_due(now).await {
                continue;
            }
            let wait = self.wait_ms(now);
            tokio::select! {
                () = self.wake.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(wait)) => {}
            }
        }
    }

    /// How long the sender may sleep before it must look again.
    fn wait_ms(&self, now_ms: u64) -> u64 {
        let next = {
            let state = self.state.lock_recover();
            state.items.iter().map(|item| item.next_at_ms).min()
        };
        next.map_or(IDLE_MS, |at| at.saturating_sub(now_ms).clamp(1, IDLE_MS))
    }

    /// Take the first due mail, and write down that its attempt has begun,
    /// so a crash mid-attempt waits like any other failure.
    fn take_due_for(&self, now_ms: u64, id: &str, revision: u64) -> Option<Taken> {
        let mut aged = Vec::new();
        let taken = {
            let mut state = self.state.lock_recover();
            // Cancellation and attempt publication share this lock: a stale
            // eligibility answer must never commit a new provider attempt.
            if state.cancellation_revision != revision {
                self.wake.notify_one();
                return None;
            }
            let items = std::mem::take(&mut state.items);
            state.bytes = 0;
            let mut taken = None;
            let mut selected = false;
            for mut item in items {
                if aged_out(&item, self.limits, now_ms)
                    || usize::try_from(item.attempts).unwrap_or(usize::MAX)
                        > self.limits.retry_ms.len()
                {
                    aged.push(item);
                    continue;
                }
                if !selected && item.id == id && item.next_at_ms <= now_ms {
                    selected = true;
                    item.attempts = item.attempts.saturating_add(1);
                    item.next_at_ms = now_ms.saturating_add(self.limits.max_wait_ms);
                    // Publication must succeed before any network side effect.
                    // Keep the lock through it so cancellation cannot resurrect
                    // a file between taking and persisting the attempt.
                    if save(&item).is_ok() {
                        taken = Some(Taken {
                            id: item.id.clone(),
                            kind: item.kind,
                            mail: item.mail.clone(),
                        });
                    } else {
                        eprintln!(
                            "tilo-room-service: an attempt was not written down; not sending"
                        );
                    }
                }
                state.bytes = state.bytes.saturating_add(item.bytes);
                state.items.push(item);
            }
            drop(state);
            taken
        };
        if !aged.is_empty() {
            eprintln!(
                "tilo-room-service: {} queued mail(s) aged out unsent",
                aged.len()
            );
            for item in &aged {
                discard_file(item);
            }
        }
        taken
    }

    #[cfg(test)]
    fn take_due(&self, now_ms: u64) -> Option<Taken> {
        let id = self
            .state
            .lock_recover()
            .items
            .iter()
            .find(|item| item.next_at_ms <= now_ms)
            .map(|item| item.id.clone())
            .unwrap_or_default();
        let revision = self.state.lock_recover().cancellation_revision;
        self.take_due_for(now_ms, &id, revision)
    }

    /// What became of the attempt on `id`.
    fn settle(
        &self,
        now_ms: u64,
        id: &str,
        kind: Kind,
        result: std::result::Result<(), SendError>,
    ) {
        match result {
            Ok(()) => self.finish(id),
            Err(SendError::Permanent(reason)) => {
                self.finish(id);
                eprintln!("tilo-room-service: {kind} was not mailed: {reason}");
            }
            Err(SendError::Retry { after, reason }) => {
                if !self.retry(now_ms, id, after) {
                    self.finish(id);
                    eprintln!("tilo-room-service: {kind} was not mailed: {reason}");
                }
            }
        }
    }

    /// Put `id` off until its next attempt; false when it has none left.
    fn retry(&self, now_ms: u64, id: &str, after: Option<Duration>) -> bool {
        {
            let mut state = self.state.lock_recover();
            let Some(item) = state.items.iter_mut().find(|item| item.id == id) else {
                return false;
            };
            let Some(backoff) = self
                .limits
                .retry_ms
                .get(usize::try_from(item.attempts.saturating_sub(1)).unwrap_or(usize::MAX))
            else {
                return false;
            };
            let asked = after.map_or(0, |after| {
                u64::try_from(after.as_millis()).unwrap_or(u64::MAX)
            });
            let delay = (*backoff).max(asked).min(self.limits.max_wait_ms);
            let next = now_ms.saturating_add(delay).max(now_ms.saturating_add(1));
            if next >= item.created_ms.saturating_add(self.limits.max_age_ms) {
                return false;
            }
            item.next_at_ms = next;
            let saved = save(item).is_ok();
            drop(state);
            saved
        }
    }

    /// Forget `id`: it is sent, refused for good, or out of time.
    fn finish(&self, id: &str) {
        let item = {
            let mut state = self.state.lock_recover();
            let Some(index) = state.items.iter().position(|item| item.id == id) else {
                return;
            };
            let item = state.items.remove(index);
            state.bytes = state.bytes.saturating_sub(item.bytes);
            item
        };
        discard_file(&item);
        if item.path.is_some()
            && let Some(dir) = &self.dir
            && let Err(error) = tilo_files::native::private::sync_directory(dir)
        {
            eprintln!(
                "tilo-room-service: could not flush the mail outbox: {}",
                reason(&error)
            );
        }
    }
}

/// Whether `name` is one of the outbox's own files, not a temporary sibling
/// a write left behind or something else in the directory.
fn is_entry_name(name: &str) -> bool {
    name.len() > 5
        && name.chars().next().is_some_and(|c| c.is_ascii_digit())
        && Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
}

/// Write `item` where a restart finds it.
fn save(item: &Item) -> std::io::Result<()> {
    let stored = Stored {
        version: VERSION,
        id: item.id.clone(),
        kind: item.kind,
        created_ms: item.created_ms,
        attempts: item.attempts,
        next_at_ms: item.next_at_ms,
        mail: item.mail.clone(),
        removal: item.removal.clone(),
    };
    let bytes = serde_json::to_vec(&stored)
        .map_err(|error| std::io::Error::other(format!("serialize a mail: {error}")))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_ENTRY_BYTES {
        return Err(std::io::Error::other(
            "serialized mail exceeds recovery limit",
        ));
    }
    match &item.path {
        Some(path) => tilo_files::native::private::write_atomic(path, &bytes),
        None => Ok(()),
    }
}

fn receipt_key(contact: &str) -> String {
    use sha2::Digest as _;
    hex::encode(sha2::Sha256::digest(
        tilo_relay_protocol::rooms::contact_key(contact),
    ))
}

fn aged_out(item: &Item, limits: Limits, now_ms: u64) -> bool {
    now_ms.saturating_sub(item.created_ms) >= limits.max_age_ms
}

/// The text of an I/O failure a log line may print: the kind, never a path
/// or what a file holds.
fn reason(error: &std::io::Error) -> String {
    format!("{:?}", error.kind())
}

fn discard_file(item: &Item) {
    if let Some(path) = &item.path {
        discard_path(path);
    }
}

fn discard_path(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "tilo-room-service: could not clear a mail outbox file: {}",
            reason(&error)
        );
    }
}

fn read_entry(path: &Path) -> Result<Item> {
    let (mut file, _) = tilo_files::native::private::open_owned_for_read(path, MAX_ENTRY_BYTES)?;
    let mut text = String::new();
    file.by_ref()
        .take(MAX_ENTRY_BYTES)
        .read_to_string(&mut text)
        .with_context(|| format!("read {}", path.display()))?;
    let stored: Stored =
        serde_json::from_str(&text).with_context(|| format!("read {}", path.display()))?;
    ensure!(
        stored.version == VERSION,
        "{}: a mail outbox entry of another version",
        path.display()
    );
    ensure!(
        stored.id.len() == 32 && stored.id.chars().all(|c| c.is_ascii_hexdigit()),
        "{}: a mail outbox entry has no delivery id",
        path.display()
    );
    ensure!(
        stored.mail.is_sendable(),
        "{}: a mail outbox entry is not a mail",
        path.display()
    );
    Ok(Item {
        id: stored.id,
        kind: stored.kind,
        created_ms: stored.created_ms,
        attempts: stored.attempts,
        next_at_ms: stored.next_at_ms,
        bytes: stored.mail.weight(),
        path: Some(path.to_path_buf()),
        mail: stored.mail,
        removal: stored.removal,
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use std::collections::VecDeque;

    /// A mailer that answers with what it was told to, then sends.
    struct Scripted {
        outcomes: Mutex<VecDeque<SendError>>,
        sent: Mutex<Vec<(String, Mail)>>,
    }

    impl Scripted {
        fn new(outcomes: impl IntoIterator<Item = SendError>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: Mutex::new(outcomes.into_iter().collect()),
                sent: Mutex::default(),
            })
        }

        fn count(&self) -> usize {
            self.sent.lock_recover().len()
        }

        fn ids(&self) -> Vec<String> {
            self.sent
                .lock_recover()
                .iter()
                .map(|(id, _)| id.clone())
                .collect()
        }
    }

    impl Mailer for Scripted {
        fn send(
            &self,
            id: &str,
            mail: Mail,
        ) -> BoxFuture<'static, std::result::Result<(), SendError>> {
            let outcome = self.outcomes.lock_recover().pop_front();
            self.sent.lock_recover().push((id.to_owned(), mail));
            Box::pin(async move { outcome.map_or(Ok(()), Err) })
        }
    }

    fn mailer(scripted: &Arc<Scripted>) -> Arc<dyn Mailer> {
        Arc::clone(scripted) as Arc<dyn Mailer>
    }

    fn a_mail() -> Mail {
        Mail {
            to: "ada@example.org".into(),
            subject: "Confirm your address".into(),
            text: "https://tilo.example/?wait-list-confirm=aa".into(),
            html: "<a href=\"https://tilo.example/?wait-list-confirm=aa\">here</a>".into(),
        }
    }

    fn files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .collect()
    }

    #[tokio::test]
    async fn a_durable_outbox_keeps_a_mail_privately_until_the_service_takes_it() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mail-outbox");
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(outbox.enqueue(Kind::Confirm, a_mail()));

        // One file, private, named by the delivery id its attempts carry.
        let kept = files(&dir);
        assert_eq!(kept.len(), 1);
        let id = outbox.state.lock_recover().items[0].id.clone();
        assert_eq!(id.len(), 32);
        assert!(kept[0].to_string_lossy().ends_with(&format!("-{id}.json")));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&kept[0]), 0o600);
        }

        // A restart reads it, and sends it with the same id.
        let restarted = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert_eq!(restarted.state.lock_recover().items.len(), 1);
        assert!(restarted.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 1);
        assert_eq!(scripted.ids(), vec![id]);
        assert!(files(&dir).is_empty());
        assert!(restarted.state.lock_recover().items.is_empty());
    }

    #[tokio::test]
    async fn a_busy_service_is_retried_with_the_same_delivery_id() {
        let scripted = Scripted::new([
            SendError::Retry {
                after: None,
                reason: "the mail service answered 503".into(),
            },
            SendError::Retry {
                after: None,
                reason: "the mail service answered 429".into(),
            },
        ]);
        let limits = Limits {
            retry_ms: &[0, 0, 0],
            ..Limits::DEFAULT
        };
        let outbox = Outbox::in_memory(mailer(&scripted), limits);
        assert!(outbox.enqueue(Kind::Confirm, a_mail()));
        for _ in 0..3 {
            assert!(
                outbox
                    .deliver_due(tilo_clock::unix_now_ms().saturating_add(60_000))
                    .await
            );
        }
        assert!(outbox.state.lock_recover().items.is_empty());
        let ids = scripted.ids();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| *id == ids[0]));
    }

    #[tokio::test]
    async fn a_failing_service_is_tried_a_bounded_number_of_times() {
        let failing = SendError::Retry {
            after: None,
            reason: "the mail service answered 503".into(),
        };
        let scripted = Scripted::new(std::iter::repeat_n(failing, 10));
        let limits = Limits {
            retry_ms: &[0],
            ..Limits::DEFAULT
        };
        let outbox = Outbox::in_memory(mailer(&scripted), limits);
        assert!(outbox.enqueue(Kind::Confirm, a_mail()));
        for _ in 0..5 {
            outbox
                .deliver_due(tilo_clock::unix_now_ms().saturating_add(60_000))
                .await;
        }
        assert_eq!(scripted.count(), 2, "one delay, so two attempts");
        assert!(outbox.state.lock_recover().items.is_empty());
    }

    #[tokio::test]
    async fn a_refused_mail_is_not_tried_again() {
        let scripted =
            Scripted::new([SendError::Permanent("the mail service answered 400".into())]);
        let outbox = Outbox::in_memory(mailer(&scripted), Limits::DEFAULT);
        assert!(outbox.enqueue(Kind::Confirm, a_mail()));
        assert!(
            outbox
                .deliver_due(tilo_clock::unix_now_ms().saturating_add(60_000))
                .await
        );
        assert_eq!(scripted.count(), 1);
        assert!(outbox.state.lock_recover().items.is_empty());
    }

    #[test]
    fn a_retry_waits_as_long_as_the_service_asks_and_keeps_the_mail() {
        let scripted = Scripted::new([]);
        let outbox = Outbox::in_memory(
            mailer(&scripted),
            Limits {
                retry_ms: &[1_000],
                max_wait_ms: 5_000,
                ..Limits::DEFAULT
            },
        );
        assert!(outbox.enqueue(Kind::Invite, a_mail()));
        let now = tilo_clock::unix_now_ms();
        let taken = outbox.take_due(now).expect("the queued mail is due");
        outbox.settle(
            now,
            &taken.id,
            taken.kind,
            Err(SendError::Retry {
                after: Some(Duration::from_secs(3)),
                reason: "the mail service answered 429".into(),
            }),
        );
        let state = outbox.state.lock_recover();
        let item = state.items.first().expect("kept for another attempt");
        assert_eq!(item.attempts, 1);
        assert_eq!(item.next_at_ms, now.saturating_add(3_000), "Retry-After");
        drop(state);
        assert_eq!(scripted.count(), 0, "nothing was sent yet");
    }

    #[test]
    fn an_outbox_with_no_room_keeps_no_more_mail() {
        let scripted = Scripted::new([]);
        let outbox = Outbox::in_memory(
            mailer(&scripted),
            Limits {
                max_items: 1,
                ..Limits::DEFAULT
            },
        );
        assert!(outbox.enqueue(Kind::Confirm, a_mail()));
        assert!(!outbox.enqueue(Kind::Confirm, a_mail()));
        assert_eq!(outbox.state.lock_recover().items.len(), 1);
    }

    /// Writes an outbox file by hand, as a previous run would have.
    fn kept(dir: &Path, name: &str, created_ms: u64, mail: Mail) -> PathBuf {
        let path = dir.join(name);
        let stored = Stored {
            version: VERSION,
            id: name
                .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-')
                .trim_end_matches(".json")
                .to_owned(),
            kind: Kind::Confirm,
            created_ms,
            attempts: 0,
            next_at_ms: created_ms,
            mail,
            removal: None,
        };
        tilo_files::native::private::write_atomic(&path, &serde_json::to_vec(&stored).unwrap())
            .unwrap();
        path
    }

    #[tokio::test]
    async fn a_mail_older_than_the_outbox_keeps_is_dropped_without_sending() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mail-outbox");
        tilo_files::native::private::directory(&dir).unwrap();
        let id = "ab".repeat(16);
        kept(&dir, &format!("0000000000000-{id}.json"), 0, a_mail());
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(files(&dir).is_empty());
        assert!(outbox.state.lock_recover().items.is_empty());
        outbox
            .deliver_due(tilo_clock::unix_now_ms().saturating_add(60_000))
            .await;
        assert_eq!(scripted.count(), 0);
    }

    #[tokio::test]
    async fn an_unreadable_entry_does_not_stop_the_outbox() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mail-outbox");
        tilo_files::native::private::directory(&dir).unwrap();
        let rubbish = dir.join("0000000000001-nomsg.json");
        std::fs::write(&rubbish, "not a mail").unwrap();
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(!rubbish.exists(), "set aside rather than retried forever");
        assert!(outbox.enqueue(Kind::Removed, a_mail()));
    }

    #[tokio::test]
    async fn recovery_enforces_bytes_and_items_with_the_enqueue_accounting() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("outbox");
        let scripted = Scripted::new([]);
        let original = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        for _ in 0..3 {
            assert!(original.enqueue(Kind::Confirm, a_mail()));
        }
        drop(original);
        let limits = Limits {
            max_bytes: a_mail().weight() * 2,
            max_items: 2,
            ..Limits::DEFAULT
        };
        let restored = Outbox::durable(&dir, mailer(&scripted), limits).unwrap();
        let state = restored.state.lock_recover();
        assert_eq!(state.items.len(), 2);
        assert_eq!(state.bytes, a_mail().weight() * 2);
        assert_eq!(files(&dir).len(), 2);
        drop(state);
        drop(restored);
        let bytes_only = Limits {
            max_bytes: a_mail().weight(),
            max_items: 20,
            ..limits
        };
        let restored = Outbox::durable(&dir, mailer(&scripted), bytes_only).unwrap();
        assert_eq!(restored.state.lock_recover().items.len(), 1);
        assert_eq!(files(&dir).len(), 1);
    }

    #[test]
    fn json_escaping_cannot_make_an_acknowledged_mail_unrecoverable() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        let mut mail = a_mail();
        mail.text = format!("link{}", "\u{0000}".repeat(200_000));
        assert!(u64::try_from(mail.weight()).unwrap() < MAX_ENTRY_BYTES);
        assert!(!outbox.enqueue(Kind::Confirm, mail));
        assert!(files(root.path()).is_empty());
        assert!(outbox.state.lock_recover().items.is_empty());
    }

    #[tokio::test]
    async fn crashes_during_attempts_do_not_reset_the_attempt_budget() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let limits = Limits {
            retry_ms: &[1],
            max_wait_ms: 1,
            ..Limits::DEFAULT
        };
        let mut now = tilo_clock::unix_now_ms();
        let outbox = Outbox::durable(root.path(), mailer(&scripted), limits).unwrap();
        assert!(outbox.enqueue(Kind::Invite, a_mail()));
        drop(outbox);
        for _ in 0..2 {
            let outbox = Outbox::durable(root.path(), mailer(&scripted), limits).unwrap();
            now += 10;
            assert!(outbox.take_due(now).is_some());
            // Simulate losing the process after recording the attempt.
        }
        let outbox = Outbox::durable(root.path(), mailer(&scripted), limits).unwrap();
        assert!(!outbox.deliver_due(now + 10).await);
        assert_eq!(scripted.count(), 0);
        assert!(outbox.state.lock_recover().items.is_empty());
        assert!(files(root.path()).is_empty());
    }

    #[tokio::test]
    async fn an_attempt_that_cannot_be_persisted_never_reaches_the_mailer() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("outbox");
        let moved = root.path().join("unavailable");
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(outbox.enqueue(Kind::Invite, a_mail()));
        std::fs::rename(&dir, &moved).unwrap();
        std::fs::write(&dir, "not a directory").unwrap();
        assert!(!outbox.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 0);
        std::fs::remove_file(&dir).unwrap();
        std::fs::rename(&moved, &dir).unwrap();
        drop(outbox);
        let restored = Outbox::durable(&dir, mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(restored.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 1);
    }

    #[tokio::test]
    async fn a_failed_attempt_write_does_not_send_a_different_unchecked_item() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        outbox.set_eligibility(Arc::new(|removal| {
            Box::pin(async move { Some(removal == "allowed") })
        }));
        assert!(outbox.enqueue_for(Kind::Invite, a_mail(), Some("allowed".into())));
        assert!(outbox.enqueue_for(Kind::Invite, a_mail(), Some("removed".into())));
        let first = outbox.state.lock_recover().items[0].path.clone().unwrap();
        std::fs::remove_file(&first).unwrap();
        std::fs::create_dir(&first).unwrap();
        assert!(!outbox.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 0);
        assert!(outbox.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(
            scripted.count(),
            0,
            "the next entry needs its own eligibility check"
        );
    }

    #[tokio::test]
    async fn durable_removal_prevents_a_retry_after_restart_even_with_another_token() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([SendError::Retry {
            after: None,
            reason: "503".into(),
        }]);
        let outbox = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        outbox.set_eligibility(Arc::new(|_| Box::pin(async { Some(true) })));
        assert!(outbox.enqueue_for(Kind::Invite, a_mail(), Some("aa".repeat(32))));
        assert!(outbox.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 1);
        // Removal used a different valid token. The durable relay state,
        // not equality with this mail's token, decides whether to retry.
        outbox.cancel(&"bb".repeat(32), None);
        drop(outbox);
        let restored = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        restored.set_eligibility(Arc::new(|_| Box::pin(async { Some(false) })));
        assert!(
            restored
                .deliver_due(tilo_clock::unix_now_ms() + 60_000)
                .await
        );
        assert_eq!(scripted.count(), 1);
        let remaining = files(root.path());
        assert!(
            remaining.is_empty(),
            "remaining outbox files: {remaining:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_while_checking_eligibility_cannot_resurrect_an_entry() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let outbox =
            Arc::new(Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap());
        let checking = Arc::new(Notify::new());
        let resume = Arc::new(Notify::new());
        outbox.set_eligibility(Arc::new({
            let checking = Arc::clone(&checking);
            let resume = Arc::clone(&resume);
            move |_| {
                let checking = Arc::clone(&checking);
                let resume = Arc::clone(&resume);
                Box::pin(async move {
                    checking.notify_one();
                    resume.notified().await;
                    Some(true)
                })
            }
        }));
        let removal = "ab".repeat(32);
        assert!(outbox.enqueue_for(Kind::Confirm, a_mail(), Some(removal.clone())));
        let sending = tokio::spawn({
            let outbox = Arc::clone(&outbox);
            async move { outbox.deliver_due(tilo_clock::unix_now_ms()).await }
        });
        checking.notified().await;
        outbox.cancel(&removal, None);
        resume.notify_one();
        assert!(sending.await.unwrap());
        assert_eq!(scripted.count(), 0);
        assert!(outbox.state.lock_recover().items.is_empty());
        let restarted = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(restarted.state.lock_recover().items.is_empty());
    }

    #[tokio::test]
    async fn cancellation_between_eligibility_and_attempt_commit_invalidates_the_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(outbox.enqueue_for(Kind::Confirm, a_mail(), Some("aa".repeat(32))));
        // The eligibility answer has arrived, but take_due_for has not yet
        // acquired the commit lock. A different token cancels in that gap.
        let (id, revision) = {
            let state = outbox.state.lock_recover();
            (state.items[0].id.clone(), state.cancellation_revision)
        };
        outbox.cancel(&"bb".repeat(32), None);
        assert!(
            outbox
                .take_due_for(tilo_clock::unix_now_ms(), &id, revision)
                .is_none()
        );
        assert_eq!(scripted.count(), 0);
        assert_eq!(outbox.state.lock_recover().items[0].attempts, 0);
        let restored = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        assert_eq!(restored.state.lock_recover().items[0].attempts, 0);
        restored.set_eligibility(Arc::new(|_| Box::pin(async { Some(false) })));
        assert!(restored.deliver_due(tilo_clock::unix_now_ms()).await);
        assert_eq!(scripted.count(), 0);
        let remaining = files(root.path());
        assert!(
            remaining.is_empty(),
            "remaining outbox files: {remaining:?}"
        );
    }

    #[test]
    fn alternate_removal_clears_pending_mail_and_receipts_by_protocol_contact_key() {
        let root = tempfile::tempdir().unwrap();
        let scripted = Scripted::new([]);
        let outbox = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        let mut mail = a_mail();
        mail.to = "Äda@Example.org".into();
        assert!(outbox.enqueue_for(Kind::Confirm, mail, Some("aa".repeat(32))));
        assert!(outbox.confirmation_receipt(" ÄDA@example.org ", true));
        assert!(outbox.confirmation_kept("äda@example.org"));
        let hash = receipt_key("äda@example.org");
        outbox.cancel(&"bb".repeat(32), Some(&hash));
        assert!(outbox.state.lock_recover().items.is_empty());
        assert!(!outbox.confirmation_kept("ÄDA@example.org"));
        let restored = Outbox::durable(root.path(), mailer(&scripted), Limits::DEFAULT).unwrap();
        assert!(restored.state.lock_recover().items.is_empty());
        assert!(!restored.confirmation_kept("äda@example.org"));
        assert_eq!(scripted.count(), 0);
    }

    #[tokio::test]
    async fn a_queued_mail_is_sent_without_being_prompted() {
        let scripted = Scripted::new([]);
        let outbox = Arc::new(Outbox::in_memory(mailer(&scripted), Limits::DEFAULT));
        outbox.start();
        assert!(outbox.enqueue(Kind::Invite, a_mail()));
        tilo_test_env::within("the mail", async {
            while scripted.count() == 0 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await;
        assert_eq!(scripted.count(), 1);
    }
}
