//! The device's one session with its person's server: what shows the person's
//! agent present on their card there, and what tells this daemon when the person stopped it from
//! that card.
//!
//! Agents' calls never do: each `tilo mcp` connection is answered by the daemon's own
//! workspaces, as the person's apps are ([`crate::daemon`]). Every connection open when the person
//! stopped the agent answers that they did; a connection that starts after it is a new
//! conversation, and the device enters again for it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ed25519_dalek::SigningKey;
use serde_json::Value;
use tilo_api::daemon::enter::{self, NOT_LOGGED_IN};
use tilo_api::daemon::unreachable_answer;
use tilo_api::direct::Direct;
use tilo_device_identity::login::Device;
use tilo_lock::MutexRecover as _;
use tilo_mcp::lines::Answer;
use tilo_session_client::device::Device as Entered;

use super::services::Daemon;
use super::workspaces::ServiceWorkspaces;

/// How long entering the workspace may take.
const ENTERING: Duration = Duration::from_secs(25);
/// What the host's session says once the person stopped this agent from its card.
const STOPPED: &str = "agent-run-stopped";
/// The longest the daemon waits, as it goes, for its session to close: the host then sees the
/// connection end at once, and takes the agent down, rather than after the connection's silence
/// ends. A session's endpoint gives its packet relay up to three seconds to hear it go, and a
/// session entered anew under the same key waits that out.
pub(crate) const CLOSING: Duration = Duration::from_secs(5);

/// This device in the workspace its login names.
pub(crate) struct Session {
    pub(crate) device: Entered,
}

impl Session {
    /// What every call says once the person stopped the agent from its card.
    fn stopped(&self) -> Option<String> {
        self.device
            .session()
            .status()
            .failures
            .into_iter()
            .find(|failure| failure.code == STOPPED)
            .map(|failure| {
                format!(
                    "{} Start a new conversation with your agent to go on.",
                    failure.message
                )
            })
    }

    /// Whether it still serves: it is live, or the person stopped its agent, which the connections
    /// open then go on hearing.
    fn serves(&self) -> bool {
        self.device.is_live() || self.stopped().is_some()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent's run ends with the session, and the host that shows it takes it down.
        self.device.close();
    }
}

/// One agent connection, numbered in the order they opened.
pub(crate) struct Visit(u64);

/// The device's session, entered when something needs it, and the agent connections it shows.
pub(crate) struct Presence {
    key: SigningKey,
    /// Where the device enters, once it knows its person's server.
    home: Arc<super::home::Home>,
    own: Arc<ServiceWorkspaces>,
    /// Tilo's API as this daemon, which answers an agent's account calls.
    api: Arc<Direct>,
    current: Mutex<Option<Arc<Session>>>,
    /// Taken while the device enters, so it enters once at a time.
    entering: tokio::sync::Mutex<()>,
    /// Where entries started from a connection run.
    runtime: tokio::runtime::Handle,
    /// How many agent connections opened.
    opened: AtomicU64,
    /// The last connection open when the person stopped the agent, and what they hear.
    stopped: Mutex<Option<(u64, String)>>,
}

impl Presence {
    /// `daemon`'s presence, entering on the runtime this is made on.
    ///
    /// # Errors
    /// The daemon's API cannot be built.
    pub(crate) fn new(daemon: &Arc<Daemon>) -> std::io::Result<Arc<Self>> {
        Ok(Arc::new(Self {
            key: daemon.key().clone(),
            home: daemon.home_source(),
            own: Arc::clone(daemon.serving()),
            api: daemon.api()?,
            current: Mutex::new(None),
            entering: tokio::sync::Mutex::new(()),
            runtime: tokio::runtime::Handle::current(),
            opened: AtomicU64::new(0),
            stopped: Mutex::new(None),
        }))
    }

    /// The device this computer is as it enters its person's workspaces, once it knows their
    /// server.
    pub(crate) fn home(&self) -> Option<Device> {
        self.home.get()
    }

    /// The session as it is now, whether or not it serves.
    pub(crate) fn current(&self) -> Option<Arc<Session>> {
        self.current.lock_recover().clone()
    }

    /// The device's session, entering the workspace when no session serves; why not, when it
    /// cannot.
    pub(crate) async fn session(&self) -> Result<Arc<Session>, String> {
        let entering = self.entering.lock().await;
        if let Some(session) = self.current().filter(|session| session.serves()) {
            return Ok(session);
        }
        // Its connection goes before another under the same key dials: a server keeps one
        // connection per key, so the two would cut each other off.
        self.close().await;
        let session = Arc::new(self.enter().await?);
        *self.current.lock_recover() = Some(Arc::clone(&session));
        drop(entering);
        Ok(session)
    }

    async fn enter(&self) -> Result<Session, String> {
        let device = self.home().ok_or_else(|| NOT_LOGGED_IN.to_owned())?;
        let lent = Some(self.own.lent());
        let entered = tokio::time::timeout(ENTERING, enter::connect(&self.key, &device, lent))
            .await
            .map_err(|_| {
                format!(
                    "Tilo couldn't reach the workspace on {} within {} seconds; it is offline, \
                     or this computer's connection is down. Try again in a minute.",
                    enter::host_of(&device.host_origin),
                    ENTERING.as_secs()
                )
            })?
            .map_err(|error| {
                enter::debug_why("entering the workspace", &error);
                enter::enter_said(&error)
            })?;
        Ok(Session { device: entered })
    }

    /// Close the session, waiting at most [`CLOSING`] for it to close.
    pub(crate) async fn close(&self) {
        let Some(session) = self.current.lock_recover().take() else {
            return;
        };
        let peer = session.device.session().clone();
        drop(session);
        let closed = async {
            while peer.status().phase != tilo_session_client::TransportPhase::Closed {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        let _ = tokio::time::timeout(CLOSING, closed).await;
    }

    /// An agent connection opens: the device enters for it when it is not in the workspace, and
    /// enters anew in place of a session whose agent the person stopped.
    pub(crate) fn visit(self: &Arc<Self>) -> Visit {
        let stopped = self.note_stop();
        let visit = Visit(self.opened.fetch_add(1, Ordering::SeqCst).saturating_add(1));
        let presence = Arc::clone(self);
        self.runtime.spawn(async move {
            if stopped {
                presence.close().await;
            }
            if let Err(why) = presence.session().await {
                tracing::debug!(%why, "this computer is not in its workspace for an agent");
            }
        });
        visit
    }

    /// Whether the person stopped the agent of the current session, keeping that every
    /// connection open by now hears it.
    fn note_stop(&self) -> bool {
        let Some(said) = self.current().and_then(|session| session.stopped()) else {
            return false;
        };
        *self.stopped.lock_recover() = Some((self.opened.load(Ordering::SeqCst), said));
        true
    }

    /// The answer to `request` on `visit` when it is not the workspace's: every call once the
    /// person stopped the agent, and the person's account (`account/…`: their workspaces and
    /// keys), which Tilo's API answers as for any key of theirs. Any other request back, for the
    /// connection's own session.
    pub(crate) fn answer_first(
        &self,
        visit: &Visit,
        request: Value,
    ) -> Result<Answer<'static>, Value> {
        self.note_stop();
        let stopped = self
            .stopped
            .lock_recover()
            .as_ref()
            .filter(|(last, _)| visit.0 <= *last)
            .map(|(_, said)| said.clone());
        if let Some(said) = stopped {
            return Ok(Answer::Overlapping(Box::pin(async move {
                Ok(unreachable_answer(&request, &said))
            })));
        }
        if !account_call(&request) {
            return Err(request);
        }
        let api = Arc::clone(&self.api);
        Ok(Answer::Overlapping(Box::pin(async move {
            Ok(Some(api.mcp(request.clone()).await.unwrap_or_else(
                |problem| unreachable_answer(&request, &problem).unwrap_or(Value::Null),
            )))
        })))
    }
}

/// Whether `request` is a tool call on the person's account (`account/…`), or a read of its
/// keys' record ([`tilo_api::account::KEYS_RECORD`]).
fn account_call(request: &Value) -> bool {
    let tool = request.pointer("/params/name").and_then(Value::as_str);
    let path = request
        .pointer("/params/arguments/path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (matches!(tool, Some("tilo_ls" | "tilo_read" | "tilo_do"))
        && path.split(['/', '#']).next() == Some("account"))
        || (tool == Some("tilo_read") && path == tilo_api::account::KEYS_RECORD)
}
