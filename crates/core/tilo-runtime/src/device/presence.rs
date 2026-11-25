//! The device's session with its person's server, which shows the person's agent present on
//! their card there.
//!
//! Agents' calls never enter the server session: each `tilo mcp` connection is answered by the
//! daemon's own workspaces, as the person's apps are ([`crate::daemon`]), and its session is
//! present in who is here through the workspaces' one presence feed
//! ([`tilo_workspace_service::workspaces::PresentDevices`]): present and waiting before it asks
//! anything, staying while its session lasts however long it goes quiet, and gone as soon as the
//! session ends or the person stops it.

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
    /// Whether it still serves.
    fn serves(&self) -> bool {
        self.device.is_live()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The agent's run ends with the session, and the host that shows it takes it down.
        self.device.close();
    }
}

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
        }))
    }

    /// The device this computer is as it enters its person's workspaces, once it knows their
    /// server.
    pub(crate) fn home(&self) -> Option<Device> {
        self.home.get_blocking()
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
        let device = self
            .home
            .get()
            .await
            .ok_or_else(|| NOT_LOGGED_IN.to_owned())?;
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
    pub(crate) fn visit(self: &Arc<Self>) {
        let presence = Arc::clone(self);
        self.runtime.spawn(async move {
            if let Err(why) = presence.session().await {
                tracing::debug!(%why, "this computer is not in its workspace for an agent");
            }
        });
    }

    /// The answer to `request` when it is not the workspace's: the person's account
    /// (`account/…`: their workspaces and keys), which Tilo's API answers as for any key of
    /// theirs. Any other request back, for the connection's own session.
    pub(crate) fn answer_first(&self, request: Value) -> Result<Answer<'static>, Value> {
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

/// Whether `request` is a tool call on the person's account (`account/…`).
fn account_call(request: &Value) -> bool {
    let tool = request.pointer("/params/name").and_then(Value::as_str);
    let path = request
        .pointer("/params/arguments/path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    matches!(tool, Some("tilo_ls" | "tilo_read" | "tilo_do"))
        && path.split(['/', '#']).next() == Some("account")
}
