//! Conditional display answers persisted on the existing serial writer.
//!
//! A queued answer carries the exact transaction observed at admission. The
//! settings source compares it after adopting external-copy writes under its
//! canonical lock; stale answers cannot revive replaced display intent.

use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, mpsc};

#[derive(Clone, Copy)]
pub(crate) enum Answer {
    Keep,
    Revert,
}

fn wake_slot() -> &'static Mutex<Option<crate::session_runtime::Wake>> {
    static WAKE: OnceLock<Mutex<Option<crate::session_runtime::Wake>>> = OnceLock::new();
    WAKE.get_or_init(Mutex::default)
}

pub(crate) fn set_wake(wake: crate::session_runtime::Wake) {
    *wake_slot().lock().unwrap_or_else(PoisonError::into_inner) = Some(wake);
}

pub(crate) struct PendingAnswer {
    result: mpsc::Receiver<std::io::Result<()>>,
}

impl PendingAnswer {
    pub(crate) fn poll(&self) -> std::io::Result<Option<()>> {
        match self.result.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(std::io::Error::other("display answer storage stopped"))
            }
        }
    }
}

pub(crate) fn answer(answer: Answer) -> std::io::Result<PendingAnswer> {
    let source = settings_store::installed_source()
        .ok_or_else(|| std::io::Error::other("conditional display storage is unavailable"))?;
    let transaction = DisplayTransaction::load()
        .ok_or_else(|| std::io::Error::other("no display change is waiting"))?;
    let allowed = match answer {
        Answer::Keep => transaction.state == DisplayTransactionState::AwaitingConfirmation,
        Answer::Revert => matches!(
            transaction.state,
            DisplayTransactionState::Requested
                | DisplayTransactionState::AwaitingConfirmation
                | DisplayTransactionState::ConfirmRequested
        ),
    };
    if !allowed {
        return Err(std::io::Error::other("no display change is waiting"));
    }
    let expected = section(&transaction)?;
    let mut changed = transaction;
    changed.state = match answer {
        Answer::Keep => DisplayTransactionState::ConfirmRequested,
        Answer::Revert => DisplayTransactionState::RevertRequested,
    };
    let values = section(&changed)?;
    let wake = wake_slot()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(Arc::clone);
    let (sender, result) = mpsc::channel();
    super::durable_writer::global().submit_ordered(move || {
        let stored = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            store_answer(source.as_ref(), &expected, values)
        }))
        .unwrap_or_else(|_| Err(std::io::Error::other("display answer storage panicked")));
        let _ = sender.send(stored);
        if let Some(wake) = wake {
            wake();
        }
    })?;
    Ok(PendingAnswer { result })
}

fn section(
    transaction: &DisplayTransaction,
) -> std::io::Result<serde_json::Map<String, serde_json::Value>> {
    serde_json::to_value(transaction)
        .map_err(std::io::Error::other)?
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("invalid display transaction"))
}

fn store_answer(
    source: &dyn settings_store::SettingsSource,
    expected: &serde_json::Map<String, serde_json::Value>,
    values: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    use settings_store::ConditionalUpdate;
    match source.compare_update_section("display_transaction", expected, values)? {
        ConditionalUpdate::Stored => Ok(()),
        ConditionalUpdate::Superseded => Err(std::io::Error::other(
            "display change was replaced before the answer was recorded",
        )),
        ConditionalUpdate::StoredWithCopyRepair(error) => {
            tracing::warn!(%error, "display answer is durable; settings copy repair remains pending");
            Ok(())
        }
    }
}
