//! The idle measurement for a shared workspace: a mounted app showing a
//! shared document nobody edits asks the document owner nothing.
//!
//! Before the reader waited for changes it re-Opened its committed read
//! every 200 ms, so the owner captured (and authorized) each route's record
//! five times a second, forever, in every app of every shared workspace. The
//! idle desktop gate cannot see this: its workspace has no native custody.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};

/// How long the app is left alone before and while it is measured.
const SETTLE: std::time::Duration = std::time::Duration::from_secs(2);
const WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

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

/// Counts what the mounted app's committed readers ask the actual owner.
#[derive(Default)]
pub(super) struct Counter {
    captures: AtomicUsize,
    checks: AtomicUsize,
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
}

/// The actual owner, every call forwarded; only counted.
struct Reads {
    inner: Arc<dyn NativeDocumentReads>,
    counter: Arc<Counter>,
}
impl NativeDocumentReads for Reads {
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
        self.inner.capture(context, revision, maximum)
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

/// Leave the mounted app alone once it shows the shared file, then count
/// what its readers ask the owner over [`WINDOW`].
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(mounted: &Mounted) {
    let counter = mounted
        .idle_reads
        .as_ref()
        .expect("the idle journey counts the first mounted app's reads");
    assert!(
        mounted.code.app.collaboration_is_connected(),
        "the mounted app is connected before it idles"
    );
    tokio::time::sleep(SETTLE).await;
    let (captures, checks) = counter.sample();
    let announcements = tilo_identity_host::policy_change::epoch();
    // The test process's own threads, sampled from /proc over the window:
    // the apps' collaboration shovels and the broker are among them.
    let wakeups = tokio::task::spawn_blocking(|| {
        tilo_test_env::idle::idle_wakeups(std::process::id(), std::time::Duration::ZERO, WINDOW)
    })
    .await
    .unwrap()
    .expect("sample the test process's threads");
    let (captures_after, checks_after) = counter.sample();
    let announced = tilo_identity_host::policy_change::epoch().wrapping_sub(announcements);
    let idle_captures = captures_after.saturating_sub(captures);
    let idle_checks = checks_after.saturating_sub(checks);
    let seconds = WINDOW.as_secs_f64();
    let per_second = |count: usize| f64::from(u32::try_from(count).unwrap_or(u32::MAX)) / seconds;
    eprintln!(
        "IDLE-SHARED-WORKSPACE owner_captures_per_s={:.2} owner_checks_per_s={:.2} announcements_per_s={:.2} reliable_signals={} window_ms={}",
        per_second(idle_captures),
        per_second(idle_checks),
        per_second(usize::try_from(announced).unwrap_or(usize::MAX)),
        tilo_identity_host::policy_change::timers_reliable()
            && tilo_identity_host::policy_change::identity_files_watched(),
        WINDOW.as_millis(),
    );
    let thread = |name: &str| {
        wakeups.rate(
            wakeups
                .rows
                .iter()
                .filter(|row| row.thread.starts_with(name))
                .map(|row| row.voluntary)
                .sum(),
        )
    };
    eprintln!(
        "IDLE-SHARED-WORKSPACE-THREADS shovel_blocking_per_s={:.2} broker_blocking_per_s={:.2}\n{wakeups}",
        thread("ui-collab-shove"),
        thread("tilo-collab-bro"),
    );
    assert_eq!(
        idle_captures, 0,
        "an idle shared document was captured {idle_captures} times in {WINDOW:?} ({idle_checks} cheap checks): is its reader polling again?"
    );
    assert!(
        mounted.code.app.collaboration_is_connected(),
        "waiting instead of polling keeps the reader connected"
    );
}
