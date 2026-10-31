//! The session's published resources as the app's UI thread reaches them:
//! the reading each stands at, the readings that follow, and the actions
//! that change them. The watch runs on the broker's worker.

use super::{
    BridgeState, PublishedCommand, SETTINGS_COMMAND_BRIDGE, SdkCommand, enqueue_sdk_command, mpsc,
};
use crate::time::Duration;
use std::sync::Arc;
use tokio::sync::mpsc as tokio_mpsc;

/// How long a change of a published resource waits for the broker's answer:
/// the worker replies as soon as the door took it, so this bounds a lost
/// connection, not slow work.
const PUBLISHED_CHANGE_TIMEOUT: Duration = Duration::from_secs(4);

/// How long opening a published resource waits for the reading its watch
/// begins at.
const PUBLISHED_OPEN_TIMEOUT: Duration = Duration::from_secs(4);

static PUBLISHED_COMMAND_BUDGET: std::sync::OnceLock<BridgeState> = std::sync::OnceLock::new();

/// One the session's published resources (`sound`, `displays`, the person's
/// settings), as the app's UI thread reaches it: the reading it stands at,
/// each later reading, and the actions that change it.
///
/// [`Self::open`] watches it on the broker's worker, so [`Self::latest`]
/// hands the newest reading without waiting on the frame thread, and
/// [`Self::invoke`] takes an action with its arguments and waits for the
/// door's answer. Clones share one watch: the last one dropped ends it.
#[derive(Clone)]
pub struct SessionResource {
    resource: Arc<str>,
    newest: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// Ends the worker's watch when the last handle is dropped.
    _watch: Arc<tokio::sync::watch::Sender<()>>,
}

impl SessionResource {
    /// Watch `resource` and wait for the reading it reads now. Blocking:
    /// call it off the frame thread, as [`Self::invoke`].
    ///
    /// # Errors
    ///
    /// No session is connected, it does not answer in time, or no service
    /// publishes `resource`.
    pub fn open(resource: &str) -> Result<Self, String> {
        let newest = Arc::new(std::sync::Mutex::new(None));
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let (cancel, cancel_rx) = tokio::sync::watch::channel(());
        let sender = SETTINGS_COMMAND_BRIDGE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .map_err(|_| "The session's services are unavailable.")?
            .as_ref()
            .and_then(tokio_mpsc::WeakSender::upgrade)
            .ok_or("Published resources need the certified collaboration service.")?;
        if !enqueue_sdk_command(
            &sender,
            SdkCommand::Published(PublishedCommand::Watch {
                resource: Arc::from(resource),
                readings: Arc::clone(&newest),
                first: first_tx,
                cancel: cancel_rx,
            }),
            PUBLISHED_COMMAND_BUDGET.get_or_init(BridgeState::new),
        ) {
            return Err("The published resource queue is unavailable.".into());
        }
        match first_rx.recv_timeout(PUBLISHED_OPEN_TIMEOUT) {
            Ok(Ok(reading)) => {
                *newest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ok(reading));
                Ok(Self {
                    resource: Arc::from(resource),
                    newest,
                    _watch: Arc::new(cancel),
                })
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err("The session did not answer.".to_string()),
        }
    }

    /// The newest reading since the last call; `None` when nothing changed.
    #[must_use]
    pub fn latest(&self) -> Option<Result<String, String>> {
        self.newest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Invoke `action` of the resource with `arguments` (none for an action
    /// that takes nothing). Blocking, bounded: call it off the frame thread.
    ///
    /// # Errors
    ///
    /// No session is connected, it does not answer in time, or the door
    /// refused the action, with what it said.
    pub fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let sender = SETTINGS_COMMAND_BRIDGE
            .get_or_init(|| std::sync::Mutex::new(None))
            .lock()
            .map_err(|_| "The session's services are unavailable.")?
            .as_ref()
            .and_then(tokio_mpsc::WeakSender::upgrade)
            .ok_or("Published resources need the certified collaboration service.")?;
        let (reply, receiver) = mpsc::sync_channel(1);
        if !enqueue_sdk_command(
            &sender,
            SdkCommand::Published(PublishedCommand::Invoke {
                resource: Arc::clone(&self.resource),
                action: action.to_owned(),
                arguments: arguments.clone(),
                reply,
            }),
            PUBLISHED_COMMAND_BUDGET.get_or_init(BridgeState::new),
        ) {
            return Err("The published resource queue is unavailable.".into());
        }
        receiver
            .recv_timeout(PUBLISHED_CHANGE_TIMEOUT)
            .map_err(|_| "The session did not answer the change.".to_string())?
    }
}
