//! Conditional display answers persisted on the existing serial writer.
//!
//! A queued answer carries the exact transaction observed at admission. The
//! settings source compares it after adopting external-copy writes under its
//! canonical lock; stale answers cannot revive replaced display intent.

use settings_store::display_tools::{DisplayTool, TrustedDisplayTools};
use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use tilo_lock::MutexRecover as _;

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
    *wake_slot().lock_recover() = Some(wake);
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

pub(crate) struct PendingLayout {
    result: mpsc::Receiver<std::io::Result<DisplayTransaction>>,
}

impl PendingLayout {
    pub(crate) fn poll(&self) -> std::io::Result<Option<DisplayTransaction>> {
        match self.result.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(std::io::Error::other("display layout storage stopped"))
            }
        }
    }
}

/// Prepare a whole-desktop layout; see [`stage_plan`].
pub(crate) fn stage_layout(
    mode: settings_store::display_mode::DisplayLayoutMode,
) -> std::io::Result<PendingLayout> {
    stage_plan(|snapshot, previous| {
        settings_store::display_mode::plan_layout(mode, &snapshot.outputs, previous)
            .map_err(std::io::Error::other)
    })
}

/// Prepare against one committed profile view and the native capabilities;
/// only an actual conditional persistence receipt makes this request effective.
/// `plan` turns the compositor's published displays and the committed layout
/// into the candidate, or says why it cannot.
pub(crate) fn stage_plan(
    plan: impl FnOnce(
        &settings_store::hardware::DisplayHardwareSnapshot,
        &settings_store::hardware::DisplayConfig,
    ) -> std::io::Result<settings_store::hardware::DisplayConfig>,
) -> std::io::Result<PendingLayout> {
    use settings_store::hardware::{DisplayHardwareSnapshot, DisplayProfiles};
    let source = settings_store::installed_source();
    let file = if source.is_none() {
        Some(settings_store::file_write_path()?)
    } else {
        None
    };
    let document = source
        .as_ref()
        .map_or_else(settings_store::load, |source| source.load());
    let snapshot = DisplayHardwareSnapshot::load()
        .ok_or_else(|| std::io::Error::other("the compositor has not published displays"))?;
    let set = settings_store::display_mode::attached_set(&snapshot.outputs);
    let profiles: DisplayProfiles = document
        .get("display_profiles")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    let previous = profiles.layout_for(&set).unwrap_or_default();
    let candidate = plan(&snapshot, &previous)?;
    let mut transaction = DisplayTransaction::prepare_request(set, candidate)?;
    transaction.previous = previous;
    let update = settings_store::DisplayUpdate::new(&document, section(&transaction)?, None);
    let wake = wake_slot().lock_recover().as_ref().map(Arc::clone);
    let (sender, result) = mpsc::channel();
    super::durable_writer::global().submit_ordered(move || {
        let stored = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let current = DisplayHardwareSnapshot::load().ok_or_else(|| {
                std::io::Error::other("display capabilities are no longer available")
            })?;
            if current.publisher_pid != snapshot.publisher_pid
                || current.publisher_start_ticks != snapshot.publisher_start_ticks
                || current.outputs != snapshot.outputs
            {
                return Err(std::io::Error::other(
                    "attached displays changed before the layout was recorded",
                ));
            }
            store_display_update(source.as_deref(), file.as_deref(), update)?;
            Ok(transaction)
        }))
        .unwrap_or_else(|_| Err(std::io::Error::other("display layout storage panicked")));
        let _ = sender.send(stored);
        if let Some(wake) = wake {
            wake();
        }
    })?;
    Ok(PendingLayout { result })
}

fn store_display_update(
    source: Option<&dyn settings_store::SettingsSource>,
    file: Option<&std::path::Path>,
    update: settings_store::DisplayUpdate,
) -> std::io::Result<()> {
    let receipt = if let Some(source) = source {
        source.compare_display_update(update)?
    } else {
        settings_store::compare_display_update_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
            update,
        )?
    };
    match receipt {
        settings_store::ConditionalUpdate::Stored => Ok(()),
        settings_store::ConditionalUpdate::Superseded => Err(std::io::Error::other(
            "display transaction or profiles changed before persistence",
        )),
        settings_store::ConditionalUpdate::StoredWithCopyRepair(error) => {
            tracing::warn!(%error, "display intent is durable; settings copy repair remains pending");
            Ok(())
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
    let wake = wake_slot().lock_recover().as_ref().map(Arc::clone);
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
    use settings_store::hardware::DISPLAY_TRANSACTION_SECTION;
    let receipt = match source {
        Some(source) => {
            source.compare_update_section(DISPLAY_TRANSACTION_SECTION, expected, values)
        }
        None => settings_store::compare_update_section_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
            DISPLAY_TRANSACTION_SECTION,
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
        .get(TrustedDisplayTools::SECTION)
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
        source.update_section_report(TrustedDisplayTools::SECTION, values)?
    } else {
        settings_store::update_section_at(
            file.ok_or_else(|| std::io::Error::other("display storage is unavailable"))?,
            TrustedDisplayTools::SECTION,
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
