//! The active measurement for a shared workspace: while a collaborator keeps
//! editing a large shared document, what does each of their edits cost the
//! app that shows it?
//!
//! The second device makes the document large, then edits it a line at a
//! time. The first device's mounted app re-reads the committed document
//! after each edit; this journey counts what its readers are handed by the
//! actual owner (bytes, owner time, whole captures) and how long each edit
//! takes to show there.
use super::*;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};

/// Lines in the large document, about 50,000 characters: one text op each.
const LINES: usize = 1_000;
/// Small edits measured after the document is large.
const EDITS: usize = 10;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_shared_document_under_remote_editing_is_reread_by_the_edit() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::ActiveReads),
    ))
    .await;
}

/// What the first mounted app's committed readers were handed by the owner.
#[derive(Default)]
pub(super) struct Meter {
    captures: AtomicU64,
    capture_bytes: AtomicU64,
    owner_ns: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    captures: u64,
    capture_bytes: u64,
    owner_ns: u64,
}
impl Sample {
    fn since(self, before: Self) -> Self {
        Self {
            captures: self.captures.saturating_sub(before.captures),
            capture_bytes: self.capture_bytes.saturating_sub(before.capture_bytes),
            owner_ns: self.owner_ns.saturating_sub(before.owner_ns),
        }
    }
}
impl Meter {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    pub(super) fn wrapper(
        this: &Arc<Self>,
    ) -> crate::shell::collab_broker::NativeReadHarnessWrapper {
        let meter = this.clone();
        Arc::new(move |inner| {
            Arc::new(Reads {
                inner,
                meter: meter.clone(),
            })
        })
    }
    fn sample(&self) -> Sample {
        Sample {
            captures: self.captures.load(Ordering::Acquire),
            capture_bytes: self.capture_bytes.load(Ordering::Acquire),
            owner_ns: self.owner_ns.load(Ordering::Acquire),
        }
    }
    fn timed<T>(&self, read: impl FnOnce() -> T) -> T {
        let started = std::time::Instant::now();
        let result = read();
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.owner_ns.fetch_add(elapsed, Ordering::AcqRel);
        result
    }
    fn handed(&self, read: &NativeDocumentRead) {
        if let NativeDocumentRead::Captured { body, .. } = read {
            self.captures.fetch_add(1, Ordering::AcqRel);
            self.capture_bytes
                .fetch_add(body.bytes().len() as u64, Ordering::AcqRel);
        }
    }
}

/// The actual owner, every call forwarded; only measured.
struct Reads {
    inner: Arc<dyn NativeDocumentReads>,
    meter: Arc<Meter>,
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
        let read = self
            .meter
            .timed(|| self.inner.capture(context, revision, maximum))?;
        self.meter.handed(&read);
        Ok(read)
    }
    fn unchanged(
        &self,
        context: &NativeDocumentReadContext,
        revision: Option<[u8; 32]>,
    ) -> Result<Option<bool>, String> {
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

/// CPU time this process's threads have used so far, in all and on the
/// threads whose names start with each of `prefixes`: both devices, the
/// relay, the brokers (`tilo-collab-bro`) and the apps' collaboration
/// shovels (`ui-collab-shove`), whose reads this journey measures.
fn thread_cpu(prefixes: &[&str]) -> (std::time::Duration, Vec<std::time::Duration>) {
    let threads = tilo_test_env::idle::sample_process(std::process::id())
        .expect("sample the test process's threads");
    let ticks = |keep: &dyn Fn(&str) -> bool| {
        let ticks: u64 = threads
            .iter()
            .filter(|thread| keep(&thread.comm))
            .map(|thread| thread.cpu_ticks)
            .sum();
        tilo_test_env::idle::CPU_TICK * u32::try_from(ticks).unwrap_or(u32::MAX)
    };
    (
        ticks(&|_| true),
        prefixes
            .iter()
            .map(|prefix| ticks(&|comm| comm.starts_with(prefix)))
            .collect(),
    )
}

/// Make the shared document large from the second device, then edit it a
/// line at a time there and measure each edit on the first device.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(
    context: &Context<'_>,
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
) {
    let meter = mounted[0]
        .active_reads
        .clone()
        .expect("the active journey measures the first mounted app's reads");
    let mut body = String::from(BODY);
    for line in 0..LINES {
        let _ = writeln!(body, "// shared line {line:05} kept by both devices");
    }
    mounted[1].invoke("replace-text", Some(&body));
    drive(context, authority, source, refreshes, mounted, &body, 2).await;
    eprintln!(
        "active journey: the shared document holds {} characters",
        body.len()
    );
    let mut latencies = Vec::with_capacity(EDITS);
    let before = meter.sample();
    let threads = ["tilo-collab-bro", "ui-collab-shove"];
    let (cpu_before, parts_before) = thread_cpu(&threads);
    for edit in 0..EDITS {
        let _ = writeln!(body, "// remote edit {edit}");
        let started = std::time::Instant::now();
        mounted[1].invoke("replace-text", Some(&body));
        drive(context, authority, source, refreshes, mounted, &body, 2).await;
        latencies.push(started.elapsed());
    }
    let (cpu_after, parts_after) = thread_cpu(&threads);
    let cpu = cpu_after.saturating_sub(cpu_before);
    let parts: Vec<_> = parts_after
        .iter()
        .zip(&parts_before)
        .map(|(after, before)| after.saturating_sub(*before))
        .collect();
    let read = meter.sample().since(before);
    latencies.sort_unstable();
    let edits = EDITS as u64;
    eprintln!(
        "ACTIVE-SHARED-DOCUMENT doc_chars={} edits={EDITS} full_captures={} reader_bytes_per_edit={} owner_read_us_per_edit={} process_cpu_ms_per_edit={} broker_cpu_ms_per_edit={} shovel_cpu_ms_per_edit={} latency_ms_p50={} latency_ms_max={}",
        body.len(),
        read.captures,
        read.capture_bytes / edits,
        read.owner_ns / edits / 1_000,
        cpu.as_millis() / u128::from(edits),
        parts[0].as_millis() / u128::from(edits),
        parts[1].as_millis() / u128::from(edits),
        latencies[EDITS / 2].as_millis(),
        latencies[EDITS - 1].as_millis(),
    );
    assert!(
        mounted
            .iter()
            .all(|app| app.code.app.collaboration_is_connected()),
        "both apps stay connected through the edits"
    );
}
