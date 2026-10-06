//! Conditional display answers persisted on the existing serial writer.
//!
//! A queued answer carries the exact transaction observed at admission. The
//! settings source compares it after adopting external-copy writes under its
//! canonical lock; stale answers cannot revive replaced display intent.

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
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
    answer_for(answer, None)
}

pub(crate) fn answer_for(
    answer: Answer,
    expected_id: Option<u64>,
) -> std::io::Result<PendingAnswer> {
    queue_answer(answer, expected_id, None)
}

pub(crate) fn answer_and_trust(id: u64, tool: DisplayTool) -> std::io::Result<PendingAnswer> {
    queue_answer(Answer::Keep, Some(id), Some(tool))
}

fn queue_answer(
    answer: Answer,
    expected_id: Option<u64>,
    trust: Option<DisplayTool>,
) -> std::io::Result<PendingAnswer> {
    let source = settings_store::installed_source();
    let file = if source.is_none() {
        Some(settings_store::file_write_path()?)
    } else {
        None
    };
    let transaction = DisplayTransaction::load()
        .ok_or_else(|| std::io::Error::other("no display change is waiting"))?;
    if expected_id.is_some_and(|id| transaction.id != id) {
        return Err(std::io::Error::other("display change was replaced"));
    }
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
            store_answer(source.as_deref(), file.as_deref(), &expected, values)?;
            if let Some(tool) = trust {
                store_trust(source.as_deref(), file.as_deref(), tool)?;
            }
            Ok(())
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
    source: Option<&dyn settings_store::SettingsSource>,
    file: Option<&std::path::Path>,
    expected: &serde_json::Map<String, serde_json::Value>,
    values: serde_json::Map<String, serde_json::Value>,
) -> std::io::Result<()> {
    use settings_store::ConditionalUpdate;
    let receipt = match source {
        Some(source) => source.compare_update_section("display_transaction", expected, values),
        None => settings_store::compare_update_section_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
            "display_transaction",
            expected,
            values,
        ),
    }?;
    match receipt {
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

/// Only reached after the exact displayed transaction accepted Keep. An old
/// or refused answer cannot grant trust to its tool. Both writes stay on the
/// existing serial worker; trust errors remain visible to the person.
fn store_trust(
    source: Option<&dyn settings_store::SettingsSource>,
    file: Option<&std::path::Path>,
    tool: DisplayTool,
) -> std::io::Result<()> {
    use settings_store::ConditionalUpdate;
    let document = match source {
        Some(source) => source.load(),
        None => settings_store::load_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
        ),
    };
    let mut tools: TrustedDisplayTools = document
        .get("display_tools")
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default();
    if !tools.insert(tool) {
        return Ok(());
    }
    let values = serde_json::to_value(tools)
        .map_err(std::io::Error::other)?
        .as_object()
        .cloned()
        .ok_or_else(|| std::io::Error::other("invalid trusted display tools"))?;
    let receipt = if let Some(source) = source {
        source.update_section_report("display_tools", values)?
    } else {
        settings_store::update_section_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
            "display_tools",
            values,
        )?;
        ConditionalUpdate::Stored
    };
    match receipt {
        ConditionalUpdate::Stored => Ok(()),
        ConditionalUpdate::StoredWithCopyRepair(error) => {
            tracing::warn!(%error, "display tool trust is durable; settings copy repair remains pending");
            Ok(())
        }
        ConditionalUpdate::Superseded => {
            Err(std::io::Error::other("display tool trust was superseded"))
        }
    }
}
