//! Deterministic idle work for a shared workspace: a mounted app showing a
//! shared document nobody edits asks the document owner nothing.
//!
//! Before the reader waited for changes it re-Opened its committed read
//! every 200 ms, so the owner captured (and authorized) each route's record
//! five times a second, forever, in every app of every shared workspace. The
//! idle desktop gate cannot see this: its workspace has no native custody.
//!
//! The test process has no identity directory, so the broker cannot rely on
//! being told when an identity file changes and re-checks a held wait every
//! 200 ms. A second journey runs in a child process given identity
//! directories to watch, as a session has. Every file-event batch must reach
//! the held reader, without a capture or a polling timer being scheduled.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_action_broker::services::{
    HeldRevision, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};

/// Independent file-change/revalidation rendezvous exercised per run.
const EVENTS: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_idle_shared_document_asks_its_owner_nothing() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::IdleReads),
    ))
    .await;
}

/// Set in the child process the announced journey runs in.
const ANNOUNCED_CHILD: &str = "TILO_TEST_IDLE_READS_ANNOUNCED";
/// With every change announced the broker checks a held read only when told:
/// once per waiting reader per announcement (the app holds at most this
/// many reads). The unannounced fallback additionally schedules timed checks.
const CHECKS_PER_ANNOUNCEMENT: usize = 4;
/// The same idle journey with identity directories to watch, in a child
/// process so its environment is its own: the broker holds each reader's
/// wait without a clock and checks only on announcements.
#[test]
fn an_idle_shared_document_with_watched_identity_files_is_checked_only_when_told() {
    if std::env::var_os(ANNOUNCED_CHILD).is_some() {
        trace_publication_hops();
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
            .block_on(Box::pin(super::super::actual_exchange(
                false,
                true,
                Some(Journey::IdleReadsAnnounced),
            )));
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("config");
    let root = directory.path().join("identity");
    std::fs::create_dir_all(config.join("tilo").join("identity")).unwrap();
    std::fs::create_dir_all(root.join("peers")).unwrap();
    let module = module_path!()
        .split_once("::")
        .map_or(module_path!(), |(_, module)| module);
    let test = format!(
        "{module}::an_idle_shared_document_with_watched_identity_files_is_checked_only_when_told"
    );
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([test.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(ANNOUNCED_CHILD, "1")
        .env("XDG_CONFIG_HOME", &config)
        .env("TILO_IDENTITY_ROOT", &root)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the announced idle journey failed in its child process: {status}"
    );
}

/// Counts what the mounted app's committed readers ask the actual owner.
#[derive(Default)]
pub(super) struct Counter {
    captures: AtomicUsize,
    checks: AtomicUsize,
    /// Reads answered with the whole document, and with only changes.
    whole: AtomicUsize,
    changes: AtomicUsize,
    waits: AtomicUsize,
    clocked_waits: AtomicUsize,
    file_events: std::sync::atomic::AtomicU64,
    observed: tokio::sync::Notify,
}
impl Counter {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub(super) fn wrapper(
        this: &Arc<Self>,
    ) -> crate::shell::collab_broker::NativeReadHarnessWrapper {
        let counter = this.clone();
        Arc::new(move |inner| {
            Arc::new(Reads {
                inner,
                counter: counter.clone(),
            })
        })
    }
    fn sample(&self) -> (usize, usize) {
        (
            self.captures.load(Ordering::Acquire),
            self.checks.load(Ordering::Acquire),
        )
    }
    /// Whole reads and changes reads handed out so far.
    fn handed(&self) -> (usize, usize) {
        (
            self.whole.load(Ordering::Acquire),
            self.changes.load(Ordering::Acquire),
        )
    }
    fn count(&self, read: &NativeDocumentRead) {
        match read {
            NativeDocumentRead::Captured { .. } => self.whole.fetch_add(1, Ordering::AcqRel),
            NativeDocumentRead::Changes { .. } => self.changes.fetch_add(1, Ordering::AcqRel),
            NativeDocumentRead::Unchanged(_) | NativeDocumentRead::Pending => 0,
        };
    }
}

/// The actual owner, every call forwarded; only counted.
struct Reads {
    inner: Arc<dyn NativeDocumentReads>,
    counter: Arc<Counter>,
}
impl NativeDocumentReads for Reads {
    fn observe_held_wait(&self, context: &NativeDocumentReadContext, announced: bool) {
        self.inner.observe_held_wait(context, announced);
        self.counter.waits.fetch_add(1, Ordering::AcqRel);
        if !announced {
            self.counter.clocked_waits.fetch_add(1, Ordering::AcqRel);
        }
        self.counter.file_events.fetch_max(
            tilo_identity_host::policy_change::identity_file_events(),
            Ordering::AcqRel,
        );
        self.counter.observed.notify_one();
    }
    fn requires_room(&self, room: &str) -> bool {
        self.inner.requires_room(room)
    }
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<runtime_collab::committed_document::CreationWorkspace>, String> {
        self.inner.prepare_application(context)
    }
    fn initialize_application(
        &self,
        context: &NativeDocumentReadContext,
        revision: [u8; 32],
        kind: &str,
        document: &[u8],
    ) -> Result<[u8; 32], String> {
        self.inner
            .initialize_application(context, revision, kind, document)
    }
    fn authorize(&self, context: &NativeDocumentReadContext) -> Result<(), String> {
        self.inner.authorize(context)
    }
    fn capture(
        &self,
        context: &NativeDocumentReadContext,
        revision: Option<[u8; 32]>,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.counter.captures.fetch_add(1, Ordering::AcqRel);
        let read = self.inner.capture(context, revision, maximum)?;
        self.counter.count(&read);
        self.counter.observed.notify_one();
        Ok(read)
    }
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        self.counter.captures.fetch_add(1, Ordering::AcqRel);
        let read = self.inner.capture_since(context, held, maximum)?;
        self.counter.count(&read);
        self.counter.observed.notify_one();
        Ok(read)
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
        self.counter.checks.fetch_add(1, Ordering::AcqRel);
        self.inner.unchanged(context, revision)
    }
    fn next_change(
        &self,
        context: &NativeDocumentReadContext,
        after: u64,
    ) -> Result<Option<u64>, String> {
        self.inner.next_change(context, after)
    }
}

/// Measure the actual broker's work across file-event/revalidation rendezvous,
/// not an elapsed idle window or Linux scheduler accounting.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(device: &Device, mounted: &mut Mounted, announced: bool) {
    let counter = mounted
        .idle_reads
        .clone()
        .expect("the idle journey counts the first mounted app's reads");
    tilo_test_env::within(
        "the original committed reader to settle",
        mounted.shown(BODY),
    )
    .await;
    tilo_test_env::within("the original reader to hold its wait", async {
        let captures = counter.captures.load(Ordering::Acquire);
        while counter.waits.load(Ordering::Acquire) == 0 {
            counter.observed.notified().await;
            assert_eq!(
                counter.captures.load(Ordering::Acquire),
                captures,
                "the settled reader must hold a wait, not repeatedly open its document"
            );
        }
    })
    .await;
    let (captures, checks) = counter.sample();
    let announcements = tilo_identity_host::policy_change::epoch();
    let waits = counter.waits.load(Ordering::Acquire);
    if announced {
        assert!(tilo_identity_host::policy_change::timers_reliable());
        assert!(tilo_identity_host::policy_change::identity_files_watched());
        // Only the child owns these directories. No identity contents are
        // fabricated or revoked; the production watcher observes file writes.
        let root = std::path::PathBuf::from(std::env::var_os("TILO_IDENTITY_ROOT").unwrap());
        for event in 0..EVENTS {
            let before = tilo_identity_host::policy_change::identity_file_events();
            std::fs::write(
                root.join("peers").join("idle-read-watch-probe"),
                event.to_string(),
            )
            .unwrap();
            tilo_test_env::within(
                "the file watcher and held reader to process the write",
                async {
                    while counter.file_events.load(Ordering::Acquire) <= before {
                        counter.observed.notified().await;
                    }
                },
            )
            .await;
        }
    } else {
        // No file watch is available in this journey. Exercise each genuine
        // announcement and wait for its broker check, without advancing time.
        for _ in 0..EVENTS {
            let before = counter.waits.load(Ordering::Acquire);
            tilo_identity_host::policy_change::announce();
            tilo_test_env::within("the held reader to check an announcement", async {
                while counter.waits.load(Ordering::Acquire) <= before {
                    counter.observed.notified().await;
                }
            })
            .await;
        }
    }
    let (captures_after, checks_after) = counter.sample();
    let announced_count =
        usize::try_from(tilo_identity_host::policy_change::epoch().wrapping_sub(announcements))
            .unwrap();
    let idle_captures = captures_after.checked_sub(captures).unwrap();
    let idle_checks = checks_after.checked_sub(checks).unwrap();
    let held_waits = counter
        .waits
        .load(Ordering::Acquire)
        .checked_sub(waits)
        .unwrap();
    let clocked_waits = counter.clocked_waits.load(Ordering::Acquire);
    eprintln!(
        "IDLE-SHARED-WORKSPACE events={EVENTS} captures={idle_captures} checks={idle_checks} announcements={announced_count} held_waits={held_waits} clocked_waits={clocked_waits}"
    );
    assert_eq!(
        idle_captures, 0,
        "held readers never reopen an unchanged document"
    );
    assert!(
        held_waits >= EVENTS,
        "every event reaches the actual held reader"
    );
    if announced {
        assert_eq!(
            clocked_waits, 0,
            "watched readers never schedule polling rechecks"
        );
        assert!(
            idle_checks
                <= CHECKS_PER_ANNOUNCEMENT
                    .checked_mul(announced_count)
                    .unwrap(),
            "each waiting reader checks at most once per actual announcement"
        );
    }
    assert!(mounted.code.app.collaboration_is_connected());
    changes_reach_the_idle_reader(device, mounted, &counter).await;
}

/// A committed edit then reaches the reader that held the document as only
/// its changes, through the actual owner, broker and app.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn changes_reach_the_idle_reader(device: &Device, mounted: &mut Mounted, counter: &Counter) {
    let owner = mounted.network.native_owner().unwrap();
    let (whole, changes) = counter.handed();
    let edited = format!("{BODY}// committed while the reader waited\n");
    super::blocked_read::commit_edit(device, &owner, BODY, &edited);
    tilo_test_env::within("the committed edit to show in the waiting app", async {
        mounted.shown(&edited).await;
    })
    .await;
    let (whole_after, changes_after) = counter.handed();
    eprintln!(
        "IDLE-SHARED-WORKSPACE-EDIT whole_reads={} changes_reads={}",
        whole_after.saturating_sub(whole),
        changes_after.saturating_sub(changes)
    );
    assert!(
        changes_after > changes,
        "the edit reached the waiting reader as its changes"
    );
    assert_eq!(
        whole_after, whole,
        "a reader that holds the document is not handed it whole for one edit"
    );
}
