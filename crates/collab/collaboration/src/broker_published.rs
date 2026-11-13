//! The session's published resources as the app's UI thread reaches them:
//! the reading each stands at, the readings that follow, and the actions
//! that change them. The watch runs on the broker's worker; the view that
//! shows a reading is woken at each one.

use super::{
    BridgeState, PUBLISHED_COMMAND_BRIDGE, PublishedCommand, SdkCommand, enqueue_sdk_command, mpsc,
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
/// door's answer. [`Self::attach`] has the watch wake the view showing the
/// reading, as a locked query's does. Clones share one watch: the last one
/// dropped ends it.
#[derive(Clone)]
pub struct SessionResource {
    resource: Arc<str>,
    newest: Arc<std::sync::Mutex<Option<Result<String, String>>>>,
    /// Woken at each reading that arrives, once a view attached one.
    wake: Arc<std::sync::Mutex<Option<tilo_rt::wake::WakeHandle>>>,
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
        let (watched, first) = Self::watch(resource, None)?;
        match first.recv_timeout(PUBLISHED_OPEN_TIMEOUT) {
            Ok(Ok(reading)) => {
                *watched
                    .newest
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ok(reading));
                Ok(watched)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err("The session did not answer.".to_string()),
        }
    }

    /// Watch `resource` and take every reading it publishes, waiting for
    /// none: a service that starts after this call is followed when it
    /// appears, and nothing is read until it does. `wake`'s owner is woken at
    /// each reading, the first one included. Blocking only while the watch is
    /// handed to the session's worker; call it off the frame thread, as
    /// [`Self::open`].
    ///
    /// # Errors
    ///
    /// No session is connected to watch through.
    pub fn follow(resource: &str, wake: tilo_rt::wake::WakeHandle) -> Result<Self, String> {
        Self::watch(resource, Some(wake)).map(|(watched, _first)| watched)
    }

    /// Register the watch `resource`, answering the handle and the channel
    /// its first reading arrives on.
    fn watch(
        resource: &str,
        first_wake: Option<tilo_rt::wake::WakeHandle>,
    ) -> Result<(Self, mpsc::Receiver<Result<String, String>>), String> {
        let newest = Arc::new(std::sync::Mutex::new(None));
        let wake = Arc::new(std::sync::Mutex::new(first_wake));
        let (first_tx, first_rx) = mpsc::sync_channel(1);
        let (cancel, cancel_rx) = tokio::sync::watch::channel(());
        let sender = PUBLISHED_COMMAND_BRIDGE
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
                wake: Arc::clone(&wake),
                first: first_tx,
                cancel: cancel_rx,
            }),
            PUBLISHED_COMMAND_BUDGET.get_or_init(BridgeState::new),
        ) {
            return Err("The published resource queue is unavailable.".into());
        }
        Ok((
            Self {
                resource: Arc::from(resource),
                newest,
                wake,
                _watch: Arc::new(cancel),
            },
            first_rx,
        ))
    }

    /// The newest reading since the last call; `None` when nothing changed.
    #[must_use]
    pub fn latest(&self) -> Option<Result<String, String>> {
        self.newest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// Wake `wake`'s owner at each reading that arrives, so a page showing
    /// the resource draws the new one instead of looking for it.
    pub fn attach(&self, wake: tilo_rt::wake::WakeHandle) {
        *self
            .wake
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(wake);
    }

    /// Invoke `action` of `resource` with `arguments`, without watching it:
    /// for a resource the app acts on and never shows, so none of its
    /// readings crosses the wire. Blocking, bounded: call it off the frame
    /// thread, as [`Self::invoke`].
    ///
    /// # Errors
    ///
    /// The session refused the action, no service publishes `resource`, or
    /// there is no session to ask.
    pub fn invoke_without_watching(
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Self::send_invoke(Arc::from(resource), action, arguments)
    }

    /// Invoke `action` of the resource with `arguments` (none for an action
    /// that takes nothing). Blocking, bounded: call it off the frame thread.
    pub fn invoke(
        &self,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        Self::send_invoke(Arc::clone(&self.resource), action, arguments)
    }

    /// Ask the session to invoke `action` of `resource` with `arguments`,
    /// waiting for its answer.
    fn send_invoke(
        resource: Arc<str>,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let sender = PUBLISHED_COMMAND_BRIDGE
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
                resource,
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
