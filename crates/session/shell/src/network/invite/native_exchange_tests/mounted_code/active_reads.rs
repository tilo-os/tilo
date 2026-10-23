//! The active measurement for a shared workspace: while a collaborator keeps
//! editing a large shared document, what does each of their edits cost the
//! app that shows it, and the host that commits it?
//!
//! A collaborator makes the document large, then edits it a line at a time,
//! each edit a signed write the actual owner commits. The mounted app
//! re-reads the committed document after each one; this journey counts what
//! its readers are handed by the owner (bytes, owner time, whole captures),
//! how long each edit takes to show there, and the CPU the compositor's
//! broker thread spends per edit, the thread that commits what the app
//! writes and answers every reader.
//!
//! Each commit used to rebuild, for every edit the governor had not yet
//! acknowledged, that edit's whole recovery history, one dependency at a
//! time: 5.7 s of broker CPU per edit at 48,000 characters (1.2 s at
//! 10,000), which starved the reader's round trips past their deadlines
//! into renewals, each a whole read. Then the app sent every edit it read
//! back to the owner, twice, as its own (once per route publishing the
//! record), and every commit wrote the owner's whole record out as JSON
//! about seven times, its outgoing bytes a line per byte: 725 ms of broker
//! CPU per edit at 48,000 characters. The app now sends nothing it only
//! read, so the broker's share is its reads, flat in the document's size;
//! the edit's own commit, which persists the record, is the process's.
use super::*;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use tilo_action_broker::services::{
    HeldRevision, NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};
use tilo_actions::Action as _;

/// Lines written per edit while the document is made large.
const LINES_PER_EDIT: usize = 250;
/// Small edits measured after the document is large.
const EDITS: usize = 10;
/// Bytes a reader may be handed per one-line edit of the large document;
/// a whole read hands it the document, tens of kilobytes.
const READER_BYTES_PER_EDIT_BUDGET: u64 = 4096;

/// A document of about 10,000 characters (200 lines, one text op each).
/// Measured in this debug harness on the shared runner, per edit: 18 ms of
/// broker CPU (1.2 s, then 247 ms before), 382 ms of process CPU (1.0 s)
/// and 241 ms to show (577 ms).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_shared_document_under_remote_editing_is_reread_by_the_edit() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::ActiveReads { lines: 200 }),
    ))
    .await;
}

/// About 48,000 characters (1,000 lines). Measured per edit: 20 ms of broker
/// CPU (5.7 s, then 725 ms before), 574 ms of process CPU (2.3 s) and 344 ms
/// to show (1.5 s).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_long_shared_document_under_remote_editing_keeps_its_broker_responsive() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::ActiveReads { lines: 1000 }),
    ))
    .await;
}

/// Broker CPU a one-line edit may cost at either size: the broker only
/// answers the app's reads, which hand it the edit.
const BROKER_MS_PER_EDIT_BUDGET: u128 = 100;
/// CPU the whole test process may spend per one-line edit, by document size
/// in lines: both devices, the relay, the app showing the edit and the
/// owner's commit of it, which persists the owner's record.
fn process_ms_per_edit_budget(lines: usize) -> u128 {
    if lines <= 200 { 800 } else { 1100 }
}

/// What the first mounted app's committed readers were handed by the owner.
#[derive(Default)]
pub(super) struct Meter {
    captures: AtomicU64,
    changes: AtomicU64,
    bytes: AtomicU64,
    owner_ns: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    captures: u64,
    changes: u64,
    bytes: u64,
    owner_ns: u64,
}
impl Sample {
    fn since(self, before: Self) -> Self {
        Self {
            captures: self.captures.saturating_sub(before.captures),
            changes: self.changes.saturating_sub(before.changes),
            bytes: self.bytes.saturating_sub(before.bytes),
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
            changes: self.changes.load(Ordering::Acquire),
            bytes: self.bytes.load(Ordering::Acquire),
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
        let (count, body) = match read {
            NativeDocumentRead::Captured { body, .. } => (&self.captures, body),
            NativeDocumentRead::Changes { body, .. } => (&self.changes, body),
            NativeDocumentRead::Unchanged(_) | NativeDocumentRead::Pending => return,
        };
        count.fetch_add(1, Ordering::AcqRel);
        self.bytes
            .fetch_add(body.bytes().len() as u64, Ordering::AcqRel);
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
        if matches!(read, NativeDocumentRead::Captured { .. }) {
            // Which reader read whole, and whether it held a revision: a
            // whole read after the document is large is what this journey
            // must not see.
            eprintln!(
                "active journey: whole read of {}/{:?} holding a revision: {}",
                context.route().app_id,
                context.route().record_id,
                revision.is_some()
            );
        }
        self.meter.handed(&read);
        Ok(read)
    }
    fn capture_since(
        &self,
        context: &NativeDocumentReadContext,
        held: &HeldRevision,
        maximum: usize,
    ) -> Result<NativeDocumentRead, String> {
        let read = self
            .meter
            .timed(|| self.inner.capture_since(context, held, maximum))?;
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
        tilo_test_env::idle::CPU_TICK.saturating_mul(u32::try_from(ticks).unwrap_or(u32::MAX))
    };
    (
        ticks(&|_| true),
        prefixes
            .iter()
            .map(|prefix| ticks(&|comm| comm.starts_with(prefix)))
            .collect(),
    )
}

/// Wait until `target` shows `body`, framing it; how long that took.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn shown(target: &mut Mounted, body: &str, what: &str) -> std::time::Duration {
    let started = std::time::Instant::now();
    tilo_test_env::within(what, async {
        loop {
            target.frame();
            assert!(
                target.code.app.collaboration_is_connected(),
                "collaboration failed while awaiting {what}"
            );
            let phase = super::blocked_read::phase(target);
            if target.has_body(body)
                && phase["committed_read_ready"] == true
                && phase["merge_ready"] == true
                && phase["pending_remote"] == false
                && phase["committed_queue"] == 0
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await;
    started.elapsed()
}

/// The measured collaborator has no mounted SDK sharing its signer. A fixture
/// write must not overtake an earlier SDK write from either mounted device.
pub(super) async fn enroll_writer(
    context: &Context<'_>,
    authority: &ActualAuthority,
    receiving_custody: &Arc<Mutex<Option<NativeWorkspaceOwner>>>,
) -> Option<Device> {
    if !matches!(context.journey, Journey::ActiveReads { .. }) {
        return None;
    }
    Some(
        Box::pin(enroll_device(
            context,
            authority,
            receiving_custody,
            84,
            "Active document collaborator",
        ))
        .await,
    )
}

pub(super) async fn close_writer(writer: Option<Device>) {
    if let Some(writer) = writer {
        writer.endpoint.close().await;
    }
}

/// Receive a genuine remote edit from the independently enrolled collaborator.
/// Its signer is never used by either mounted SDK, so a queued SDK write cannot
/// be made stale by a later synthetic sequence committed ahead of it.
fn commit_edit(device: &Device, owner: &NativeWorkspaceOwner, base: &str, expected: &str) {
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    assert_ne!(
        device.identity.device_cert.device_public_key,
        owner
            .with_current(room, |view| Ok(view.device().device_public_key))
            .unwrap(),
        "the measured remote collaborator must not share the mounted reader's signer"
    );
    let changes = owner
        .with_current(room, |view| {
            let mut document =
                automerge::AutoCommit::load(&view.peer().document(APP, cold_code::RECORD).unwrap())
                    .unwrap();
            document.set_actor(automerge::ActorId::from(
                device.identity.device_cert.device_public_key.to_vec(),
            ));
            let before = document.get_heads();
            code_editor::actions::text_edit_action(FILE, base, expected)
                .apply(&mut document)
                .unwrap();
            collab_types::CommitNamed::commit_edit(
                &mut document,
                cold_code::PROJECT_KIND,
                "buffers",
            );
            Ok(document.save_after(&before))
        })
        .unwrap();
    let message = cold_code::unordered_document_message(
        room,
        changes,
        &device.identity.device_signing_key,
        SystemTime::now(),
    );
    owner
        .receive(
            room,
            tilo_peer_state::TransportOrigin::RelayDelta(
                device.identity.device_cert.device_public_key,
            ),
            message,
            SystemTime::now(),
        )
        .expect("the native owner must admit the signed collaborator edit");
    assert!(
        has_committed_body(owner, expected),
        "the received edit must be committed, not pending"
    );
}

/// Make the shared document large, then edit it a line at a time, each a
/// signed write the owner commits, and measure what each edit costs the
/// mounted app's committed readers and how long it takes to show there.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(device: &Device, target: &mut Mounted, lines: usize) {
    let meter = target
        .active_reads
        .clone()
        .expect("the active journey measures the mounted app's reads");
    let owner = target.network.native_owner().unwrap();
    // A visible body alone does not establish the reader's applied revision.
    // Finish opening the original committed route before any synthetic edit.
    shown(target, BODY, "the initial shared document read to settle").await;
    // Written in edits a signed text edit can carry (16 KiB each).
    let mut body = String::from(BODY);
    for batch in (0..lines).collect::<Vec<_>>().chunks(LINES_PER_EDIT) {
        let base = body.clone();
        for line in batch {
            let _ = writeln!(body, "// shared line {line:05} kept by every collaborator");
        }
        commit_edit(device, &owner, &base, &body);
    }
    shown(target, &body, "the large shared document to show").await;
    eprintln!(
        "active journey: the shared document holds {} characters",
        body.len()
    );
    let mut latencies = Vec::with_capacity(EDITS);
    let before = meter.sample();
    let threads = ["tilo-collab-bro", "ui-collab-shove"];
    let (cpu_before, parts_before) = thread_cpu(&threads);
    for edit in 0..EDITS {
        let base = body.clone();
        let _ = writeln!(body, "// collaborator edit {edit}");
        let started = std::time::Instant::now();
        commit_edit(device, &owner, &base, &body);
        shown(target, &body, "the edit to show").await;
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
    let per_edit = |total: u128| total.checked_div(u128::from(edits)).unwrap_or(0);
    let bytes_per_edit = read.bytes.checked_div(edits).unwrap_or(0);
    let broker_ms_per_edit = per_edit(parts.first().map_or(0, std::time::Duration::as_millis));
    let process_ms_per_edit = per_edit(cpu.as_millis());
    let latency_ms_p50 = latencies
        .get(EDITS / 2)
        .map_or(0, std::time::Duration::as_millis);
    eprintln!(
        "ACTIVE-SHARED-DOCUMENT doc_chars={} edits={EDITS} full_captures={} changes={} reader_bytes_per_edit={bytes_per_edit} owner_read_us_per_edit={} process_cpu_ms_per_edit={} broker_cpu_ms_per_edit={} shovel_cpu_ms_per_edit={} latency_ms_p50={} latency_ms_max={}",
        body.len(),
        read.captures,
        read.changes,
        per_edit(u128::from(read.owner_ns)) / 1_000,
        process_ms_per_edit,
        broker_ms_per_edit,
        per_edit(parts.get(1).map_or(0, std::time::Duration::as_millis)),
        latency_ms_p50,
        latencies.last().map_or(0, std::time::Duration::as_millis),
    );
    assert!(
        target.code.app.collaboration_is_connected(),
        "the app stays connected through the edits"
    );
    assert_eq!(
        read.captures, 0,
        "an edit of a document the reader holds is read as its changes, never whole"
    );
    // How long an edit takes to show is reported, not budgeted: on a loaded
    // runner it measures the scheduler. A reader woken by the commit reads
    // each edit once; one that polled or renewed would read more.
    assert_eq!(
        read.changes, edits,
        "each edit is read once, when its commit wakes the reader"
    );
    assert!(
        bytes_per_edit <= READER_BYTES_PER_EDIT_BUDGET,
        "each one-line edit handed the reader {bytes_per_edit} bytes (budget {READER_BYTES_PER_EDIT_BUDGET}); the document is {} characters",
        body.len()
    );
    assert!(
        broker_ms_per_edit <= BROKER_MS_PER_EDIT_BUDGET,
        "each one-line edit cost the broker thread {broker_ms_per_edit} ms of CPU (budget {BROKER_MS_PER_EDIT_BUDGET} ms); the document is {} characters",
        body.len()
    );
    let budget = process_ms_per_edit_budget(lines);
    assert!(
        process_ms_per_edit <= budget,
        "each one-line edit cost the process {process_ms_per_edit} ms of CPU (budget {budget} ms); the document is {} characters",
        body.len()
    );
}
