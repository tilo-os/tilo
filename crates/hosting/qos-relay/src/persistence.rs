//! Ordered, bounded state work. The owner awaits publication before dequeuing
//! its successor; callers only wait on receipts and cannot cancel admitted work.
use crate::{
    store::{StoreError, WorkspaceStore},
    workspace::SharedWorkspace,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
};
use tokio::sync::{Semaphore, mpsc, oneshot};

const QUEUE_MESSAGES: usize = 32;
const GLOBAL_MESSAGES: usize = 128;
const GLOBAL_BYTES: usize = 256 * 1024 * 1024;
const BLOCKING_WORKERS: usize = 4;

type Job = Box<dyn FnOnce(Arc<Mutex<State>>) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;
#[derive(Clone)]
pub(crate) struct Persistence {
    tx: mpsc::Sender<Job>,
}
pub(crate) struct State {
    pub(crate) store: WorkspaceStore,
    pub(crate) doc: Option<automerge::AutoCommit>,
    sequence: Option<u64>,
}
pub(crate) fn error(message: impl Into<String>) -> StoreError {
    StoreError::Io(std::io::Error::other(message.into()))
}

impl State {
    pub(crate) fn next_sequence(&mut self) -> Result<u64, StoreError> {
        let path = self.store.root().join("state-sequence");
        let previous = match self.sequence {
            Some(previous) => previous,
            None => match std::fs::read(&path) {
                Ok(bytes) => u64::from_le_bytes(
                    bytes
                        .try_into()
                        .map_err(|_| error("invalid state sequence"))?,
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
                Err(e) => return Err(e.into()),
            },
        };
        let next = previous
            .checked_add(1)
            .ok_or_else(|| error("state sequence exhausted"))?
            .max(crate::peer::now_ns());
        // Reserve before any data publication, including across restart.
        atomic_file::write(&path, &next.to_le_bytes())?;
        std::fs::File::open(self.store.root())?.sync_all()?;
        self.sequence = Some(next);
        Ok(next)
    }
    pub(crate) fn would_materialize(&self, bytes: &[u8]) -> bool {
        self.doc.as_ref().is_some_and(
            |current| matches!(current.clone().load_incremental(bytes), Ok(n) if n > 0),
        )
    }
    pub(crate) fn materialize(&mut self, bytes: &[u8]) -> bool {
        let Some(current) = &self.doc else {
            return false;
        };
        let mut candidate = current.clone();
        if !matches!(candidate.load_incremental(bytes), Ok(n) if n > 0) {
            return false;
        }
        let saved = candidate.save();
        // Historical best-effort materialization: malformed/opaque bytes do
        // not alter the document; failed snapshot writes do not discard queues.
        let _ = self.store.save_snapshot(&saved);
        self.doc = Some(candidate);
        true
    }
    pub(crate) fn heads(&mut self) -> Vec<[u8; 32]> {
        self.doc
            .as_mut()
            .map(|doc| doc.get_heads().into_iter().map(|h| h.0).collect())
            .unwrap_or_default()
    }
    pub(crate) fn changes(&mut self, heads: &[[u8; 32]]) -> Vec<u8> {
        self.doc
            .as_mut()
            .map(|doc| {
                doc.save_after(
                    &heads
                        .iter()
                        .copied()
                        .map(automerge::ChangeHash)
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_default()
    }
}

pub(crate) async fn handle(workspace: &SharedWorkspace) -> Persistence {
    let mut runtime = workspace.lock().await;
    runtime.persistence.clone().unwrap_or_else(|| {
        let (tx, mut rx) = mpsc::channel::<Job>(QUEUE_MESSAGES);
        let state = Arc::new(Mutex::new(State {
            store: runtime.store.clone(),
            doc: runtime.doc.take(),
            sequence: None,
        }));
        tokio::spawn(async move {
            while let Some(job) = rx.recv().await {
                job(state.clone()).await;
            }
        });
        let handle = Persistence { tx };
        runtime.persistence = Some(handle.clone());
        handle
    })
}

impl Persistence {
    /// Nonwaiting admission: includes queued and executing work in both bounds.
    /// `bytes` charges owned input, encoding and decoded coedit body; replay output remains
    /// the explicitly eager 2c seam, not covered by this input-byte budget.
    pub(crate) fn submit<T, F, C, CF>(
        &self,
        bytes: usize,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut State) -> Result<T, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        self.submit_prepared(
            bytes,
            async { Ok(()) },
            move |state, ()| operation(state),
            completion,
        )
    }

    /// Preparation runs in owner order, after the preceding publication and
    /// before blocking persistence. Its result must not retain routing guards.
    pub(crate) fn submit_prepared<P, PF, T, F, C, CF>(
        &self,
        bytes: usize,
        preparation: PF,
        operation: F,
        completion: C,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError>
    where
        P: Send + 'static,
        PF: Future<Output = Result<P, StoreError>> + Send + 'static,
        T: Send + 'static,
        F: FnOnce(&mut State, P) -> Result<T, StoreError> + Send + 'static,
        C: FnOnce(Result<T, StoreError>) -> CF + Send + 'static,
        CF: Future<Output = Result<T, StoreError>> + Send + 'static,
    {
        static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        static BYTES: OnceLock<Arc<Semaphore>> = OnceLock::new();
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let slot = SLOTS
            .get_or_init(|| Arc::new(Semaphore::new(GLOBAL_MESSAGES)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| error("state message backpressure"))?;
        let byte_count =
            u32::try_from(bytes.max(1)).map_err(|_| error("state byte backpressure"))?;
        let bytes = BYTES
            .get_or_init(|| Arc::new(Semaphore::new(GLOBAL_BYTES)))
            .clone()
            .try_acquire_many_owned(byte_count)
            .map_err(|_| error("state byte backpressure"))?;
        let (tx, rx) = oneshot::channel();
        let queued = std::time::Instant::now();
        self.tx
            .try_send(Box::new(move |state| {
                Box::pin(async move {
                    let (_slot, _bytes) = (slot, bytes);
                    let result = match preparation.await {
                        Err(error) => Err(error),
                        Ok(prepared) => match WORKERS
                            .get_or_init(|| Arc::new(Semaphore::new(BLOCKING_WORKERS)))
                            .clone()
                            .acquire_owned()
                            .await
                        {
                            Ok(permit) => tokio::task::spawn_blocking(move || {
                                let _permit = permit;
                                let start = std::time::Instant::now();
                                let result = operation(
                                    &mut state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                                    prepared,
                                );
                                if diagnostics() {
                                    tracing::info!(
                                        phase = "state_persist",
                                        charged_bytes = byte_count,
                                        queue_us =
                                            u64::try_from(start.duration_since(queued).as_micros())
                                                .unwrap_or(u64::MAX),
                                        persist_us = u64::try_from(start.elapsed().as_micros())
                                            .unwrap_or(u64::MAX)
                                    );
                                }
                                result
                            })
                            .await
                            .unwrap_or_else(|e| Err(error(e.to_string()))),
                            Err(e) => Err(error(e.to_string())),
                        },
                    };
                    // The owner serializes this asynchronous authorization/publication
                    // step as well as the blocking transaction, even if tx is closed.
                    let result = completion(result).await;
                    let _ = tx.send(result);
                })
            }))
            .map_err(|_| error("workspace state backpressure"))?;
        Ok(rx)
    }
    pub(crate) fn read<T: Send + 'static>(
        &self,
        bytes: usize,
        operation: impl FnOnce(&mut State) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T, StoreError>>, StoreError> {
        self.submit(bytes, operation, |result| async { result })
    }
}
pub(crate) async fn receipt<T>(
    rx: oneshot::Receiver<Result<T, StoreError>>,
) -> Result<T, StoreError> {
    rx.await.map_err(|e| error(e.to_string()))?
}
pub(crate) fn diagnostics() -> bool {
    std::env::var_os("QOS_RELAY_PHASE_DIAGNOSTICS").is_some()
}

/// Shared live deliveries charge their single Arc payload once process-wide.
pub(crate) fn delivery_budget(
    bytes: usize,
) -> Result<Arc<tokio::sync::OwnedSemaphorePermit>, StoreError> {
    static LIVE: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let bytes = u32::try_from(bytes.max(1)).map_err(|_| error("live state byte backpressure"))?;
    LIVE.get_or_init(|| Arc::new(Semaphore::new(128 * 1024 * 1024)))
        .clone()
        .try_acquire_many_owned(bytes)
        .map(Arc::new)
        .map_err(|_| error("live state byte backpressure"))
}
