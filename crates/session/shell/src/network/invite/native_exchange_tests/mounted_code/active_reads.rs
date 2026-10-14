//! Count production work while an independently signed collaborator edits a
//! mounted shared document. Recovery must use the retained change history,
//! reads must deliver only changes, and the SDK must not publish what it read.
//! The collaborator commits through its own owner before the mounted owner
//! receives its signed edit. Recovery extends that retained local publication;
//! the receiving compositor encodes the new file and checks the old authority,
//! without separate encodings for size checks and the new-file digest.
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

/// A document of about 48,000 characters (1,000 lines).
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

/// What the first mounted app's committed readers were handed by the owner.
#[derive(Default)]
pub(super) struct Meter {
    captures: AtomicU64,
    changes: AtomicU64,
    bytes: AtomicU64,
}
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    captures: u64,
    changes: u64,
    bytes: u64,
}
impl Sample {
    fn since(self, before: Self) -> Self {
        Self {
            captures: self.captures.saturating_sub(before.captures),
            changes: self.changes.saturating_sub(before.changes),
            bytes: self.bytes.saturating_sub(before.bytes),
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
        }
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
        let read = self.inner.capture(context, revision, maximum)?;
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
        let read = self.inner.capture_since(context, held, maximum)?;
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

/// Frame only when the production app wake announces background work.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn shown(
    target: &mut Mounted,
    wakes: &mut tokio::sync::mpsc::UnboundedReceiver<()>,
    body: &str,
    what: &str,
) {
    tilo_test_env::within(what, async {
        loop {
            target.frame();
            assert!(
                target.code.app.collaboration_is_connected(),
                "collaboration failed while awaiting {what}"
            );
            let phase = super::blocked_read::phase(target);
            let body_matches = target.has_body(body);
            if body_matches
                && phase["committed_read_ready"] == true
                && phase["merge_ready"] == true
                && phase["pending_remote"] == false
                && phase["committed_queue"] == 0
            {
                break;
            }
            wakes
                .recv()
                .await
                .expect("the mounted wake notifier remains open");
        }
    })
    .await;
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
fn commit_edit(
    device: &Device,
    owner: &NativeWorkspaceOwner,
    base: &str,
    expected: &str,
) -> tilo_joined_room::WorkSnapshot {
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
    let (committed, mut work) = device.owner.measure_work_for_test(|| {
        device.owner.commit_document(
            tilo_joined_room::testing::request(&message).unwrap(),
            SystemTime::now(),
        )
    });
    committed.expect("the collaborator's owner must commit and retain its signed edit");
    let (received, receiving_work) = owner.measure_work_for_test(|| {
        owner.receive(
            room,
            tilo_peer_state::TransportOrigin::RelayDelta(
                device.identity.device_cert.device_public_key,
            ),
            message,
            SystemTime::now(),
        )
    });
    received.expect("the mounted owner must admit the committed collaborator edit");
    work = added_work(work, receiving_work);
    // Persistence on the receiving compositor, independent of unrelated
    // governance admissions on its background threads.
    work.record_serializations = receiving_work.record_serializations;
    assert!(
        has_committed_body(owner, expected),
        "the received edit must be committed, not pending"
    );
    work
}

/// Make the shared document large, then commit and deliver signed edits
/// through both owners. Count synchronous owner work separately from unrelated
/// background governance; count all publications by the mounted reader.
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
    let (send, mut wakes) = tokio::sync::mpsc::unbounded_channel();
    let network_send = send.clone();
    target.network.set_wake(Arc::new(move || {
        let _ = network_send.send(());
    }));
    target
        .code
        .app
        .controlled_bind_wake_for_test(move |_| {
            let _ = send.send(());
        })
        .expect("the mounted app owns an unbound production wake scope");
    shown(
        target,
        &mut wakes,
        BODY,
        "the initial shared document read to settle",
    )
    .await;
    // Written in edits a signed text edit can carry (16 KiB each).
    let mut body = String::from(BODY);
    for batch in (0..lines).collect::<Vec<_>>().chunks(LINES_PER_EDIT) {
        let base = body.clone();
        for line in batch {
            let _ = writeln!(body, "// shared line {line:05} kept by every collaborator");
        }
        commit_edit(device, &owner, &base, &body);
    }
    shown(
        target,
        &mut wakes,
        &body,
        "the large shared document to show",
    )
    .await;
    eprintln!(
        "active journey: the shared document holds {} characters",
        body.len()
    );
    let before = meter.sample();
    let mut work = tilo_joined_room::WorkSnapshot::default();
    let publications_before = target
        .code
        .app
        .controlled_outbound_document_mutations_for_test();
    for edit in 0..EDITS {
        let base = body.clone();
        let _ = writeln!(body, "// collaborator edit {edit}");
        let edit_work = commit_edit(device, &owner, &base, &body);
        work = added_work(work, edit_work);
        shown(target, &mut wakes, &body, "the edit to show").await;
    }
    let publications = target
        .code
        .app
        .controlled_outbound_document_mutations_for_test()
        .checked_sub(publications_before)
        .expect("publication counter is monotonic");
    let read = meter.sample().since(before);
    assert!(
        target.code.app.collaboration_is_connected(),
        "the app stays connected through the edits"
    );
    assert_work(work, read, publications, lines, body.len());
}

fn added_work(
    a: tilo_joined_room::WorkSnapshot,
    b: tilo_joined_room::WorkSnapshot,
) -> tilo_joined_room::WorkSnapshot {
    tilo_joined_room::WorkSnapshot {
        recovery_dependencies: a
            .recovery_dependencies
            .checked_add(b.recovery_dependencies)
            .unwrap(),
        recovery_changes: a.recovery_changes.checked_add(b.recovery_changes).unwrap(),
        record_serializations: a
            .record_serializations
            .checked_add(b.record_serializations)
            .unwrap(),
    }
}

fn assert_work(
    work: tilo_joined_room::WorkSnapshot,
    read: Sample,
    publications: u64,
    lines: usize,
    characters: usize,
) {
    let dependencies = work.recovery_dependencies;
    let recovered = work.recovery_changes;
    let serializations = work.record_serializations;
    let bytes_per_edit = read.bytes.checked_div(EDITS as u64).unwrap();
    eprintln!(
        "ACTIVE-SHARED-DOCUMENT doc_chars={} edits={EDITS} full_captures={} changes={} reader_bytes_per_edit={bytes_per_edit} recovery_dependencies={dependencies} recovery_changes={recovered} record_serializations={serializations} sdk_publications={publications}",
        characters, read.captures, read.changes,
    );
    // Each extension checks its causal metadata once: six dependencies for
    // the first small edit, then one more per edit. Extra construction batches
    // add one dependency each. Only the new change is reconstructed.
    let batches = lines.div_ceil(LINES_PER_EDIT) as u64;
    let expected_dependencies = (EDITS as u64)
        .checked_mul((EDITS as u64).checked_add(5).unwrap())
        .unwrap()
        .checked_div(2)
        .unwrap()
        .checked_add(
            batches
                .checked_sub(1)
                .unwrap()
                .checked_mul(EDITS as u64)
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        dependencies, expected_dependencies,
        "recovery extension visits causal metadata once"
    );
    assert_eq!(
        recovered, EDITS as u64,
        "recovery history reconstructs only each new change"
    );
    assert_eq!(
        serializations,
        (EDITS as u64).checked_mul(2).unwrap(),
        "each received edit encodes the new file and checks the old authority"
    );
    assert_eq!(
        publications, 0,
        "the mounted SDK must never publish a change it only read"
    );
    assert_eq!(
        read.captures, 0,
        "an edit of a document the reader holds is read as its changes, never whole"
    );
    assert_eq!(
        read.changes, EDITS as u64,
        "each settled edit is delivered once as changes"
    );
    assert!(
        bytes_per_edit <= READER_BYTES_PER_EDIT_BUDGET,
        "each one-line edit handed the reader {bytes_per_edit} bytes (budget {READER_BYTES_PER_EDIT_BUDGET}); the document is {characters} characters"
    );
}
