//! A workspace writer may delay capture acknowledgement without changing custody.
use super::*;
use automerge::AutoCommit;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tilo_action_broker::services::{
    NativeDocumentRead, NativeDocumentReadContext, NativeDocumentReads,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_renews_after_acknowledgement_waits_for_workspace_writer() {
    trace_publication_hops();
    Box::pin(super::super::actual_exchange(
        false,
        true,
        Some(Journey::AcknowledgementRecovery),
    ))
    .await;
}

pub(super) struct Gate {
    tree: crate::shell::state::WorkspaceTreeHandle,
    expected: Mutex<Option<String>>,
    after_capture: AtomicBool,
    observed: AtomicUsize,
}
impl Gate {
    pub(super) fn new(owner: &NativeWorkspaceOwner) -> Arc<Self> {
        Arc::new(Self {
            tree: crate::shell::native_workspace_owner::test_tree_of(owner),
            expected: Mutex::new(None),
            after_capture: AtomicBool::new(false),
            observed: AtomicUsize::new(0),
        })
    }
    pub(super) fn arm(&self, expected: &str) {
        *self.expected.lock().unwrap() = Some(expected.into());
    }
    pub(super) fn wrapper(
        this: &Arc<Self>,
    ) -> crate::shell::collab_broker::NativeReadHarnessWrapper {
        let gate = this.clone();
        Arc::new(move |inner| {
            Arc::new(Reads {
                inner,
                gate: gate.clone(),
            })
        })
    }
    fn captured(&self, read: &NativeDocumentRead) {
        let NativeDocumentRead::Captured { body, .. } = read else {
            return;
        };
        let mut expected = self.expected.lock().unwrap();
        if expected.as_ref().is_some_and(|expected| {
            cold_code::body(&AutoCommit::load(body.bytes()).unwrap()) == *expected
        }) {
            *expected = None;
            self.after_capture.store(true, Ordering::Release);
        }
    }
    pub(super) fn assert_observed(&self) {
        assert_eq!(
            self.observed.load(Ordering::Acquire),
            1,
            "the actual second native capture must reach its workspace mode writer"
        );
        assert!(self.expected.lock().unwrap().is_none());
    }
}
struct Reads {
    inner: Arc<dyn NativeDocumentReads>,
    gate: Arc<Gate>,
}
impl NativeDocumentReads for Reads {
    fn requires_room(&self, room: &str) -> bool {
        if !self.gate.after_capture.swap(false, Ordering::AcqRel) {
            return self.inner.requires_room(room);
        }
        self.gate.observed.fetch_add(1, Ordering::AcqRel);
        let tree = self.gate.tree.clone();
        let (held, observed) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            let _writer = tree.write();
            held.send(()).unwrap();
            // Just beyond the unchanged acknowledgement deadline, within the
            // unchanged original-root renewal deadline. No forged RPC reply.
            std::thread::sleep(std::time::Duration::from_millis(2200));
        });
        tilo_test_env::recv(
            "the actual workspace writer before acknowledgement mode lookup",
            &observed,
        );
        let required = self.inner.requires_room(room);
        writer.join().unwrap();
        required
    }
    fn prepare_application(
        &self,
        context: &NativeDocumentReadContext,
    ) -> Result<Option<runtime_collab::committed_document::CreationWorkspace>, String> {
        self.inner.prepare_application(context)
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
        self.gate.captured(&read);
        Ok(read)
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
}
