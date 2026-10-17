//! Device-local restart review. Opening an installer never adopts a persona.
use super::{
    super::prepared_identity::PreparedIdentity,
    draft::{DraftStatus, DraftStore, IdentityKind, PublicChoices, ReadyDraft},
};
use std::sync::{Arc, mpsc};

pub(super) struct Resumed {
    pub prepared: Arc<PreparedIdentity>,
    pub choices: PublicChoices,
    pub kind: IdentityKind,
    pub draft: ReadyDraft,
    pub metadata: runtime_identity::IdentityMetadata,
}
enum Answer {
    Reviewed(DraftStore, DraftStatus),
    Resumed(Resumed),
    Retired,
    ChosenNew,
    Checkpoint(ReadyDraft),
}
#[derive(Default)]
pub(super) struct Restart {
    store: Option<DraftStore>,
    worker: Option<mpsc::Receiver<Result<Answer, ()>>>,
    state: &'static str,
    queued: Option<(Arc<PreparedIdentity>, PublicChoices)>,
    checkpoint: Option<ReadyDraft>,
    queued_checkpoint: Option<(
        super::accounts::checkpoint::Receipt,
        Option<super::accounts::checkpoint::Receipt>,
    )>,
}
impl Restart {
    pub fn inspect(&mut self) -> Result<(), &'static str> {
        self.start("checking", || {
            let store = DraftStore::for_session().map_err(|_| ())?;
            let status = store.review().map_err(|_| ())?;
            Ok(Answer::Reviewed(store, status))
        })
    }
    pub fn busy(&self) -> bool {
        self.worker.is_some()
    }
    pub fn blocks(&self) -> bool {
        !matches!(
            self.state,
            "" | "missing" | "resumed" | "retired" | "saving"
        )
    }
    pub fn state(&self) -> &'static str {
        if self.state.is_empty() {
            "missing"
        } else {
            self.state
        }
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"state":self.state(), "checking":self.busy()})
    }
    pub fn resume(&mut self) -> Result<(), &'static str> {
        let store = self.store.clone().ok_or("installer-restart-unavailable")?;
        self.start("resuming", move || {
            if let DraftStatus::Pending(pending) = store.review().map_err(|_| ())? {
                pending.recover_created_new().map_err(|_| ())?;
            }
            let DraftStatus::Ready(ready) = store.review().map_err(|_| ())? else {
                return Err(());
            };
            let prepared = ready.resume().map_err(|_| ())?;
            let metadata = prepared.metadata().map_err(|_| ())?;
            Ok(Answer::Resumed(Resumed {
                prepared,
                choices: ready.choices().clone(),
                kind: ready.kind(),
                metadata,
                draft: ready,
            }))
        })
    }
    pub fn retire(&mut self) -> Result<(), &'static str> {
        let store = self.store.clone();
        self.start("retiring", move || {
            let store = match store {
                Some(store) => store,
                None => DraftStore::for_session().map_err(|_| ())?,
            };
            store.retire().map_err(|_| ())?;
            Ok(Answer::Retired)
        })
    }
    pub fn record_checkpoint(
        &mut self,
        receipt: super::accounts::checkpoint::Receipt,
        expected: Option<super::accounts::checkpoint::Receipt>,
    ) -> Result<(), &'static str> {
        if self.busy() {
            self.queued_checkpoint = Some((receipt, expected));
            return Ok(());
        }
        self.start("saving", move || {
            let store = DraftStore::for_session().map_err(|_| ())?;
            store
                .record_account_checkpoint(&receipt, expected.as_ref())
                .map_err(|_| ())?;
            let DraftStatus::Ready(ready) = store.review().map_err(|_| ())? else {
                return Err(());
            };
            Ok(Answer::Checkpoint(ready))
        })
    }
    pub fn take_checkpoint(&mut self) -> Option<ReadyDraft> {
        self.checkpoint.take()
    }
    pub fn retire_for_choice(&mut self) -> Result<(), &'static str> {
        self.queued = None;
        self.start("saving", || {
            DraftStore::for_session()
                .map_err(|_| ())?
                .retire()
                .map_err(|_| ())?;
            Ok(Answer::ChosenNew)
        })
    }
    pub fn mark_changed(&mut self) {
        self.state = "stale";
        self.queued = None;
        self.queued_checkpoint = None;
        self.checkpoint = None;
        self.worker = None;
    }
    pub fn choices_changed(&mut self, prepared: Arc<PreparedIdentity>, choices: PublicChoices) {
        self.queued = Some((prepared, choices));
    }
    fn start(
        &mut self,
        state: &'static str,
        work: impl FnOnce() -> Result<Answer, ()> + Send + 'static,
    ) -> Result<(), &'static str> {
        if self.busy() {
            return Err("installer-restart-busy");
        }
        imager_backend::prepared_identity::require_supported()
            .map_err(|_| "installer-restart-unavailable")?;
        let (tx, rx) = mpsc::channel();
        self.worker = Some(rx);
        self.state = state;
        std::thread::spawn(move || {
            let _ = tx.send(work());
        });
        Ok(())
    }
    pub fn poll(&mut self) -> Option<Result<Option<Resumed>, ()>> {
        if self.worker.is_none()
            && let Some((receipt, expected)) = self.queued_checkpoint.take()
        {
            let _ = self.record_checkpoint(receipt, expected);
        }
        if self.worker.is_none()
            && !self.blocks()
            && let Some((prepared, choices)) = self.queued.take()
        {
            let store = self.store.clone();
            let _ = self.start("saving", move || {
                let store = match store {
                    Some(store) => store,
                    None => DraftStore::for_session().map_err(|_| ())?,
                };
                store
                    .update_choices_for(&prepared, &choices)
                    .map_err(|_| ())?;
                let status = store.review().map_err(|_| ())?;
                Ok(Answer::Reviewed(store, status))
            });
        }
        let result = match self.worker.as_ref()?.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        let saving = self.state == "saving";
        self.worker = None;
        match result {
            Ok(Answer::Reviewed(store, status)) => {
                self.store = Some(store);
                self.state = if saving {
                    "resumed"
                } else {
                    match status {
                        DraftStatus::Missing => "missing",
                        DraftStatus::Pending(_) => "pending",
                        DraftStatus::Ready(_) => "available",
                    }
                };
                Some(Ok(None))
            }
            Ok(Answer::Resumed(resumed)) => {
                self.state = "resumed";
                Some(Ok(Some(resumed)))
            }
            Ok(Answer::ChosenNew) => {
                self.state = "missing";
                Some(Ok(None))
            }
            Ok(Answer::Retired) => {
                self.state = "retired";
                self.queued = None;
                Some(Ok(None))
            }
            Ok(Answer::Checkpoint(ready)) => {
                self.checkpoint = Some(ready);
                self.state = "resumed";
                Some(Ok(None))
            }
            Err(()) => {
                self.state = if saving { "resumed" } else { "unavailable" };
                Some(Err(()))
            }
        }
    }
}
