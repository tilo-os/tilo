//! The `org.bluez.Agent1` pairing agent.
//!
//! `BlueZ` does not *tell* a desktop how pairing is going — it **asks**: `bluetoothd` makes a
//! blocking D-Bus call into a registered object and waits for the pairing decision, so this
//! cannot be a `bluetoothctl` wrapper (there's no CLI shape for "the daemon calls you and
//! waits"). This object does no deciding itself — it translates each `BlueZ` question into a
//! [`PairingRequest`], sends it to whoever owns the screen, and turns the [`PairingReply`]
//! back into a return value or D-Bus error. The compositor draws the prompt; this is the wire.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};
use tracing::{debug, warn};
use zbus::zvariant::ObjectPath;
use zbus::{Connection, DBusError, interface};

use crate::model::{PairingPrompt, PairingReply, PairingRequest, address_from_path, reply_answers};

/// Where the agent object lives on our own connection.
pub const AGENT_PATH: &str = "/org/tilo/BluetoothAgent";

/// The I/O capability we advertise. `KeyboardDisplay` is the honest answer for a desktop
/// (screen to show a passkey, keyboard to type one), unlocking the strongest Secure Simple
/// Pairing modes; claiming less would silently downgrade to "just works".
pub const AGENT_CAPABILITY: &str = "KeyboardDisplay";

const BLUEZ_DEST: &str = "org.bluez";
const AGENT_MANAGER_PATH: &str = "/org/bluez";
const AGENT_MANAGER_IFACE: &str = "org.bluez.AgentManager1";

/// How long the user has to answer before we tell `BlueZ` the request expired. `bluetoothd`
/// gives an agent 60s (`REQUEST_TIMEOUT` in its `src/agent.c`) before abandoning the call
/// itself; answering just inside that window lets the daemon learn *why* pairing stopped.
pub const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(55);

/// Maximum number of unanswered/display-only prompts allowed to wait for the UI.
///
/// A `BlueZ` agent normally has one active request, but display-passkey updates and a broken or
/// hostile daemon can arrive faster than the compositor drains them.  Eight events bounds the
/// queue while leaving room for the complete six-digit display-progress sequence and a small
/// amount of scheduling jitter.
///
/// Together with [`MAX_DEVICE_PATH_BYTES`], [`MAX_DEVICE_NAME_BYTES`], and the prompt limits,
/// this caps queued string storage at 8 × (1024 + 248 + 64) = 10,688 bytes (plus fixed Rust
/// bookkeeping).  Pending answer handles use the same count limit, so a consumer that drains
/// events but never answers cannot move the bound from the queue into agent state.
pub const AGENT_EVENT_CAPACITY: usize = 8;

/// Bluetooth's legacy PIN value is at most 16 octets.  Enforcing that at the D-Bus boundary
/// keeps a daemon-controlled string out of the event queue when it is not a valid PIN anyway.
pub const MAX_PIN_CODE_BYTES: usize = 16;

/// A `BlueZ` service UUID is normally 4, 8, or 36 bytes.  This permits future textual forms but
/// still prevents an arbitrary D-Bus string from becoming queued UI state.
pub const MAX_SERVICE_UUID_BYTES: usize = 64;

/// `BlueZ` aliases are user-visible names, not an unbounded metadata transport.  248 bytes is the
/// Bluetooth local-name limit and bounds the copy carried in each queued request.
pub const MAX_DEVICE_NAME_BYTES: usize = 248;

/// Number of aliases retained for prompts.  This is a display cache, so refusing a new entry
/// under discovery floods is safe: the prompt falls back to the device address instead.
pub const MAX_DEVICE_NAME_ENTRIES: usize = 256;

/// Object paths originate on D-Bus.  They are identifiers, so one KiB is already far beyond a
/// practical `BlueZ` device path; refusing longer paths bounds each queued request.
pub const MAX_DEVICE_PATH_BYTES: usize = 1024;

/// Errors this agent returns to `BlueZ`. The names matter: `bluetoothd` branches on
/// `org.bluez.Error.Rejected` (user said no) vs `org.bluez.Error.Canceled` (nobody
/// answered), reporting them differently to the device.
#[derive(Debug, DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum AgentError {
    /// Transport-level failures, carried through rather than dressed up as a `BlueZ` error name.
    #[zbus(error)]
    ZBus(zbus::Error),
    /// The user declined.
    Rejected(String),
    /// Nobody was there to answer, or the answer did not fit the question.
    Canceled(String),
}

/// What the agent emits towards the UI.
#[derive(Debug)]
pub enum AgentEvent {
    /// `BlueZ` is asking. Send a [`PairingReply`] on `reply` to unblock it. Dropping the sender
    /// is read as [`PairingReply::Reject`].
    Ask {
        request: PairingRequest,
        reply: PairingReplySender,
    },
    /// `BlueZ` withdrew the pending request (the remote gave up, or pairing failed elsewhere).
    /// Any prompt on screen should be dismissed.
    Cancelled,
    /// `BlueZ` released the agent — it is no longer registered.
    Released,
}

/// The receiving end, held by whoever draws the prompt.
pub struct AgentEvents {
    events: mpsc::Receiver<AgentEvent>,
    lifecycle: watch::Receiver<Lifecycle>,
    closed: bool,
}

impl AgentEvents {
    /// Receive the next prompt or lifecycle notification.  Lifecycle changes take priority over
    /// queued prompts, so `Cancel`/`Release` can never be trapped behind a saturated queue.
    pub async fn recv(&mut self) -> Option<AgentEvent> {
        if self.closed {
            return self.events.recv().await;
        }
        if self.lifecycle.has_changed().unwrap_or(false) {
            return Some(lifecycle_event(*self.lifecycle.borrow_and_update()));
        }

        tokio::select! {
            biased;
            changed = self.lifecycle.changed() => {
                match changed {
                    Ok(()) => Some(lifecycle_event(*self.lifecycle.borrow_and_update())),
                    // The agent has gone away.  There can still be an event queued before it
                    // dropped, so let the bounded queue drain normally.
                    Err(_) => self.events.recv().await,
                }
            }
            event = self.events.recv() => event,
        }
    }

    /// Stop accepting new queued events and drain the events already admitted.
    pub fn close(&mut self) {
        self.events.close();
        self.closed = true;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Lifecycle {
    #[default]
    Active,
    Cancelled,
    Released,
}

fn lifecycle_event(lifecycle: Lifecycle) -> AgentEvent {
    match lifecycle {
        // An unexpected reset is handled as cancellation: keeping a stale consent prompt visible
        // is less safe than asking the user to start pairing again.
        Lifecycle::Active | Lifecycle::Cancelled => AgentEvent::Cancelled,
        Lifecycle::Released => AgentEvent::Released,
    }
}

/// A single-use answer channel for an [`AgentEvent::Ask`].
///
/// Unlike exposing a raw `oneshot::Sender`, this handle has an internal copy retained by the
/// agent.  A `BlueZ` `Cancel` or `Release` can therefore reject an already-displayed prompt even
/// when the event queue is full or the UI is slow to process its cancellation notification.
pub struct PairingReplySender {
    inner: Arc<Mutex<Option<oneshot::Sender<PairingReply>>>>,
    /// The UI-owned sender rejects on drop.  Agent-owned clones only retain the ability to
    /// cancel; they must not make a prompt look dismissed while they remain alive.
    reject_on_drop: bool,
}

impl Clone for PairingReplySender {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            reject_on_drop: false,
        }
    }
}

impl fmt::Debug for PairingReplySender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PairingReplySender").finish()
    }
}

impl PairingReplySender {
    /// Create a responder/receiver pair for embedding an agent event in another UI boundary.
    /// Production callers receive responders from [`AgentEvent::Ask`]; this is primarily useful
    /// for adapters and tests that need the same single-use cancellation semantics.
    pub fn channel() -> (Self, oneshot::Receiver<PairingReply>) {
        let (tx, rx) = oneshot::channel();
        (
            Self {
                inner: Arc::new(Mutex::new(Some(tx))),
                reject_on_drop: true,
            },
            rx,
        )
    }

    /// Answer once.  Returns the unsent reply if `BlueZ` already cancelled the request or the
    /// agent stopped listening.
    ///
    /// # Errors
    ///
    /// Returns the original reply if this prompt was already answered, cancelled, or its receiver
    /// was dropped.
    pub fn send(&self, reply: PairingReply) -> Result<(), PairingReply> {
        let Ok(mut sender) = self.inner.lock() else {
            return Err(reply);
        };
        let Some(sender) = sender.take() else {
            return Err(reply);
        };
        sender.send(reply)
    }

    fn reject(&self) {
        let _ = self.send(PairingReply::Reject);
    }
}

impl Drop for PairingReplySender {
    fn drop(&mut self) {
        if self.reject_on_drop {
            self.reject();
        }
    }
}

/// Display names for device paths, kept up to date by the client so the agent never has to
/// call back into `bluetoothd` while `bluetoothd` is waiting on it.
#[derive(Clone)]
pub struct DeviceNames {
    entries: Arc<Mutex<HashMap<String, String>>>,
    capacity: usize,
}

impl Default for DeviceNames {
    fn default() -> Self {
        Self::with_capacity(MAX_DEVICE_NAME_ENTRIES)
    }
}

impl DeviceNames {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_capacity(capacity: usize) -> Self {
        assert!(capacity > 0, "device name cache must have capacity");
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            capacity,
        }
    }

    pub fn set(&self, path: &str, name: &str) {
        if path.len() > MAX_DEVICE_PATH_BYTES || name.len() > MAX_DEVICE_NAME_BYTES {
            warn!(
                target: "tilo::bluez",
                path_bytes = path.len(),
                name_bytes = name.len(),
                "refusing oversized Bluetooth device name cache entry"
            );
            self.remove(path);
            return;
        }
        if let Ok(mut m) = self.entries.lock() {
            if !m.contains_key(path) && m.len() >= self.capacity {
                warn!(
                    target: "tilo::bluez",
                    capacity = self.capacity,
                    "Bluetooth device name cache is full; using address fallback"
                );
                return;
            }
            m.insert(path.to_string(), name.to_string());
        }
    }

    pub fn remove(&self, path: &str) {
        if let Ok(mut m) = self.entries.lock() {
            m.remove(path);
        }
    }

    /// Best-effort display name: the cached alias, else the address decoded from the object
    /// path, else the path itself — never empty, so a prompt always has something to name.
    pub fn get(&self, path: &str) -> String {
        if let Ok(m) = self.entries.lock()
            && let Some(n) = m.get(path).filter(|n| !n.is_empty())
        {
            return n.clone();
        }
        address_from_path(path).unwrap_or_else(|| "Unknown Bluetooth device".to_string())
    }
}

/// The D-Bus object `BlueZ` calls into.
pub struct PairingAgent {
    tx: mpsc::Sender<AgentEvent>,
    names: DeviceNames,
    timeout: Duration,
    active_replies: Mutex<HashMap<u64, PairingReplySender>>,
    next_request_id: AtomicU64,
    event_capacity: usize,
    lifecycle: watch::Sender<Lifecycle>,
    released: AtomicBool,
}

impl PairingAgent {
    /// Build the agent and the receiver the UI drains.
    pub fn new(names: DeviceNames) -> (Self, AgentEvents) {
        Self::with_timeout(names, DEFAULT_ANSWER_TIMEOUT)
    }

    pub fn with_timeout(names: DeviceNames, timeout: Duration) -> (Self, AgentEvents) {
        Self::with_event_capacity(names, timeout, AGENT_EVENT_CAPACITY)
    }

    fn with_event_capacity(
        names: DeviceNames,
        timeout: Duration,
        event_capacity: usize,
    ) -> (Self, AgentEvents) {
        assert!(
            event_capacity > 0,
            "an agent event queue must have capacity"
        );
        let (tx, events) = mpsc::channel(event_capacity);
        let (lifecycle, lifecycle_rx) = watch::channel(Lifecycle::Active);
        (
            Self {
                tx,
                names,
                timeout,
                active_replies: Mutex::new(HashMap::new()),
                next_request_id: AtomicU64::new(0),
                event_capacity,
                lifecycle,
                released: AtomicBool::new(false),
            },
            AgentEvents {
                events,
                lifecycle: lifecycle_rx,
                closed: false,
            },
        )
    }

    /// Push a question to the UI and wait for the answer. Every failure mode collapses to a
    /// D-Bus error rather than a hang: no listener, a dropped reply channel, a reply that
    /// doesn't answer the question, or nobody answering in time.
    async fn ask(
        &self,
        device: &ObjectPath<'_>,
        prompt: PairingPrompt,
    ) -> Result<PairingReply, AgentError> {
        let path = device.as_str();
        let request = self.request(path, prompt.clone())?;
        debug!(target: "tilo::bluez", device = %path, prompt = ?prompt, "pairing prompt raised");
        if self.released.load(Ordering::Acquire) {
            return Err(AgentError::Rejected("pairing agent released".into()));
        }
        let (reply_tx, reply_rx) = PairingReplySender::channel();
        let Ok(request_id) =
            self.next_request_id
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
        else {
            warn!(target: "tilo::bluez", device = %path, "pairing request ID space exhausted; refusing request");
            return Err(AgentError::Rejected(
                "pairing request ID space exhausted".into(),
            ));
        };
        if let Err(error) = self.insert_reply(request_id, reply_tx.clone()) {
            warn!(target: "tilo::bluez", device = %path, capacity = self.event_capacity, "too many unanswered pairing requests; refusing request");
            return Err(error);
        }
        match self.tx.try_send(AgentEvent::Ask {
            request,
            reply: reply_tx,
        }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(AgentEvent::Ask { reply, .. })) => {
                self.remove_reply(request_id);
                reply.reject();
                warn!(target: "tilo::bluez", device = %path, capacity = self.event_capacity, "pairing UI queue saturated; refusing request");
                return Err(AgentError::Rejected("pairing UI is busy".into()));
            }
            Err(mpsc::error::TrySendError::Closed(AgentEvent::Ask { reply, .. })) => {
                self.remove_reply(request_id);
                reply.reject();
                warn!(target: "tilo::bluez", device = %path, "no pairing UI attached; refusing");
                return Err(AgentError::Rejected("no pairing UI attached".into()));
            }
            // `Ask` is the only event sent on this channel; keeping this arm makes a future
            // variant addition fail closed instead of accidentally discarding a request.
            Err(_) => return Err(AgentError::Rejected("pairing UI unavailable".into())),
        }
        let reply = tokio::time::timeout(self.timeout, reply_rx).await;
        self.remove_reply(request_id);
        let reply = match reply {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => {
                warn!(target: "tilo::bluez", device = %path, "pairing prompt dismissed without an answer");
                return Err(AgentError::Rejected("prompt dismissed".into()));
            }
            Err(_) => {
                warn!(target: "tilo::bluez", device = %path, "pairing prompt timed out");
                return Err(AgentError::Canceled("no answer in time".into()));
            }
        };
        if !reply_answers(&prompt, &reply) {
            warn!(target: "tilo::bluez", device = %path, ?reply, "reply does not answer the prompt");
            return Err(AgentError::Canceled(
                "reply did not answer the prompt".into(),
            ));
        }
        if matches!(reply, PairingReply::Reject) {
            return Err(AgentError::Rejected("declined".into()));
        }
        Ok(reply)
    }

    /// One-way notification — nothing to wait for.
    fn notify(&self, device: &ObjectPath<'_>, prompt: PairingPrompt) {
        let path = device.as_str();
        let Ok(request) = self.request(path, prompt) else {
            warn!(target: "tilo::bluez", device = %path, "refusing oversized display-only pairing prompt");
            return;
        };
        let (reply, _reply_rx) = PairingReplySender::channel();
        match self.tx.try_send(AgentEvent::Ask { request, reply }) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!(target: "tilo::bluez", device = %path, capacity = self.event_capacity, "pairing UI queue saturated; discarding display-only update");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!(target: "tilo::bluez", device = %path, "no pairing UI attached for display-only update");
            }
        }
    }

    fn request(&self, path: &str, prompt: PairingPrompt) -> Result<PairingRequest, AgentError> {
        if path.len() > MAX_DEVICE_PATH_BYTES {
            warn!(target: "tilo::bluez", path_bytes = path.len(), "refusing oversized Bluetooth device path");
            return Err(AgentError::Canceled(
                "Bluetooth device path is too large".into(),
            ));
        }
        if !prompt_is_bounded(&prompt) {
            warn!(target: "tilo::bluez", prompt = ?prompt, "refusing oversized Bluetooth pairing payload");
            return Err(AgentError::Canceled(
                "Bluetooth pairing payload is too large".into(),
            ));
        }
        Ok(PairingRequest {
            device_path: path.to_string(),
            device_name: self.names.get(path),
            prompt,
        })
    }

    fn insert_reply(&self, id: u64, reply: PairingReplySender) -> Result<(), AgentError> {
        let Ok(mut replies) = self.active_replies.lock() else {
            return Err(AgentError::Rejected(
                "pairing agent state unavailable".into(),
            ));
        };
        if replies.len() >= self.event_capacity {
            return Err(AgentError::Rejected("pairing UI is busy".into()));
        }
        replies.insert(id, reply);
        Ok(())
    }

    fn remove_reply(&self, id: u64) {
        if let Ok(mut replies) = self.active_replies.lock() {
            replies.remove(&id);
        }
    }

    fn reject_active_replies(&self) {
        let replies = self
            .active_replies
            .lock()
            .map(|mut replies| std::mem::take(&mut *replies))
            .unwrap_or_default();
        for reply in replies.into_values() {
            reply.reject();
        }
    }

    fn set_lifecycle(&self, lifecycle: Lifecycle) {
        self.reject_active_replies();
        self.lifecycle.send_replace(lifecycle);
    }
}

impl Drop for PairingAgent {
    fn drop(&mut self) {
        // Object-server removal during compositor shutdown does not invoke the D-Bus `Release`
        // callback.  Complete local requests as refusals before their retained handles disappear.
        self.reject_active_replies();
        self.lifecycle.send_replace(Lifecycle::Released);
    }
}

fn prompt_is_bounded(prompt: &PairingPrompt) -> bool {
    match prompt {
        PairingPrompt::DisplayPinCode { pin } => pin.len() <= MAX_PIN_CODE_BYTES,
        PairingPrompt::AuthorizeService { uuid } => uuid.len() <= MAX_SERVICE_UUID_BYTES,
        _ => true,
    }
}

#[interface(name = "org.bluez.Agent1")]
impl PairingAgent {
    /// `BlueZ` is done with us.
    fn release(&self) {
        debug!(target: "tilo::bluez", "agent released by bluez");
        self.released.store(true, Ordering::Release);
        self.set_lifecycle(Lifecycle::Released);
    }

    /// Legacy pairing: the user types a PIN (often `0000`).
    async fn request_pin_code(&self, device: ObjectPath<'_>) -> Result<String, AgentError> {
        match self.ask(&device, PairingPrompt::RequestPinCode).await? {
            PairingReply::PinCode(pin) => Ok(pin),
            other => Err(AgentError::Canceled(format!(
                "expected a PIN, got {other:?}"
            ))),
        }
    }

    /// Legacy pairing: show a PIN for the user to type on the other device.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "device's ObjectPath type and by-value form are dictated by the org.bluez.Agent1 \
                  D-Bus method signature this interface implements"
    )]
    fn display_pin_code(&self, device: ObjectPath<'_>, pincode: String) {
        self.notify(&device, PairingPrompt::DisplayPinCode { pin: pincode });
    }

    /// The user reads a passkey off the other device and types it here.
    async fn request_passkey(&self, device: ObjectPath<'_>) -> Result<u32, AgentError> {
        match self.ask(&device, PairingPrompt::RequestPasskey).await? {
            PairingReply::Passkey(k) => Ok(k),
            other => Err(AgentError::Canceled(format!(
                "expected a passkey, got {other:?}"
            ))),
        }
    }

    /// Show a passkey for the user to type on the other device. `entered` is how many digits
    /// the remote has acknowledged so far, so the prompt can show progress.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "device's ObjectPath type and by-value form are dictated by the org.bluez.Agent1 \
                  D-Bus method signature this interface implements"
    )]
    fn display_passkey(&self, device: ObjectPath<'_>, passkey: u32, entered: u16) {
        self.notify(&device, PairingPrompt::DisplayPasskey { passkey, entered });
    }

    /// Numeric comparison — the strongest common flow. Both ends show the same six digits and
    /// the user confirms they match.
    async fn request_confirmation(
        &self,
        device: ObjectPath<'_>,
        passkey: u32,
    ) -> Result<(), AgentError> {
        self.ask(&device, PairingPrompt::ConfirmPasskey { passkey })
            .await
            .map(|_| ())
    }

    /// "Just works" pairing: no number to compare, so the only defence is the user knowing they
    /// asked for this.
    async fn request_authorization(&self, device: ObjectPath<'_>) -> Result<(), AgentError> {
        self.ask(&device, PairingPrompt::ConfirmPairing)
            .await
            .map(|_| ())
    }

    /// An already-paired device wants a profile it has not been authorised for. Trusted devices
    /// never reach here — `BlueZ` answers for them.
    async fn authorize_service(
        &self,
        device: ObjectPath<'_>,
        uuid: String,
    ) -> Result<(), AgentError> {
        self.ask(&device, PairingPrompt::AuthorizeService { uuid })
            .await
            .map(|_| ())
    }

    /// The request went away. Dismiss whatever is on screen.
    fn cancel(&self) {
        debug!(target: "tilo::bluez", "bluez cancelled the pending pairing request");
        if !self.released.load(Ordering::Acquire) {
            self.set_lifecycle(Lifecycle::Cancelled);
        }
    }
}

/// A registered agent. Dropping this does **not** unregister — call
/// [`AgentRegistration::unregister`], which is awaited, so the daemon isn't left holding a
/// dead object path.
pub struct AgentRegistration {
    connection: Connection,
    path: String,
}

impl AgentRegistration {
    /// Serve `agent` at [`AGENT_PATH`] on `connection`, register it with `BlueZ`, and ask to
    /// become the default agent — which routes prompts for pairings the user starts from
    /// anywhere (including `bluetoothctl` or the remote device) to this desktop.
    ///
    /// # Errors
    ///
    /// Returns an error if serving the object or `RegisterAgent` fails (for
    /// example, another agent is already registered at this path).
    pub async fn register(connection: &Connection, agent: PairingAgent) -> zbus::Result<Self> {
        Self::register_at(connection, agent, AGENT_PATH, AGENT_CAPABILITY).await
    }

    /// Register at an explicit path and capability. Tests use this to run several agents on one bus.
    ///
    /// # Errors
    ///
    /// Returns an error if `path` is not a valid object path, or if serving
    /// the object or the `RegisterAgent` D-Bus call fails.
    pub async fn register_at(
        connection: &Connection,
        agent: PairingAgent,
        path: &str,
        capability: &str,
    ) -> zbus::Result<Self> {
        let object_path = ObjectPath::try_from(path.to_string())?;
        connection.object_server().at(&object_path, agent).await?;
        let manager = zbus::Proxy::new(
            connection,
            BLUEZ_DEST,
            AGENT_MANAGER_PATH,
            AGENT_MANAGER_IFACE,
        )
        .await?;
        manager
            .call_method("RegisterAgent", &(&object_path, capability))
            .await?;
        // Best effort: another agent may legitimately hold the default slot; pairing still works
        // through us for pairings we initiate.
        if let Err(e) = manager
            .call_method("RequestDefaultAgent", &(&object_path,))
            .await
        {
            warn!(target: "tilo::bluez", error = %e, "could not become the default pairing agent");
        }
        debug!(target: "tilo::bluez", path, capability, "pairing agent registered");
        Ok(Self {
            connection: connection.clone(),
            path: path.to_string(),
        })
    }

    /// Unregister from `BlueZ` and drop the object.
    ///
    /// # Errors
    ///
    /// Returns an error if the `UnregisterAgent` D-Bus call fails or removing
    /// the served object fails.
    pub async fn unregister(self) -> zbus::Result<()> {
        let object_path = ObjectPath::try_from(self.path.clone())?;
        let manager = zbus::Proxy::new(
            &self.connection,
            BLUEZ_DEST,
            AGENT_MANAGER_PATH,
            AGENT_MANAGER_IFACE,
        )
        .await?;
        let unregistered = manager
            .call_method("UnregisterAgent", &(&object_path,))
            .await;
        self.connection
            .object_server()
            .remove::<PairingAgent, _>(&object_path)
            .await?;
        unregistered?;
        Ok(())
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn agent() -> (PairingAgent, AgentEvents) {
        let names = DeviceNames::new();
        names.set("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", "Studio Headphones");
        PairingAgent::with_timeout(names, Duration::from_millis(200))
    }

    fn dev() -> ObjectPath<'static> {
        ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap()
    }

    #[tokio::test]
    async fn pairing_prompts_reach_the_ui_and_answers_return_to_bluez() {
        // Phase `confirmation_prompt_reaches_the_ui_and_accepting_unblocks_bluez`.
        // The prompt reaching the UI carries the passkey and the device's display name, and
        // accepting returns `Ok` to `BlueZ`.
        {
            let (a, mut rx) = agent();
            let ui = tokio::spawn(async move {
                let AgentEvent::Ask { request, reply } = rx.recv().await.unwrap() else {
                    panic!("expected an Ask")
                };
                assert_eq!(request.device_name, "Studio Headphones");
                assert_eq!(
                    request.prompt,
                    PairingPrompt::ConfirmPasskey { passkey: 123_456 }
                );
                reply.send(PairingReply::Accept).unwrap();
            });
            a.request_confirmation(dev(), 123_456)
                .await
                .expect("accepted");
            ui.await.unwrap();
        }
        // Phase `declining_returns_rejected`.
        // Declining returns `org.bluez.Error.Rejected`, which aborts the pairing rather than
        // leaving it hanging.
        {
            let (a, mut rx) = agent();
            tokio::spawn(async move {
                let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                    panic!()
                };
                reply.send(PairingReply::Reject).unwrap();
            });
            let err = a.request_confirmation(dev(), 1).await.unwrap_err();
            assert!(matches!(err, AgentError::Rejected(_)), "{err:?}");
        }
        // Phase `typed_passkey_is_returned_verbatim`.
        // A typed passkey comes back to `BlueZ` as the u32 it asked for.
        {
            let (a, mut rx) = agent();
            tokio::spawn(async move {
                let AgentEvent::Ask { request, reply } = rx.recv().await.unwrap() else {
                    panic!()
                };
                assert_eq!(request.prompt, PairingPrompt::RequestPasskey);
                reply.send(PairingReply::Passkey(4321)).unwrap();
            });
            assert_eq!(a.request_passkey(dev()).await.unwrap(), 4321);
        }
        // Phase `typed_pin_is_returned_verbatim`.
        {
            let (a, mut rx) = agent();
            tokio::spawn(async move {
                let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                    panic!()
                };
                reply.send(PairingReply::PinCode("0000".into())).unwrap();
            });
            assert_eq!(a.request_pin_code(dev()).await.unwrap(), "0000");
        }
        // Phase `display_passkey_returns_at_once_and_still_notifies`.
        // Display-only prompts return immediately and still tell the UI what to put on screen.
        {
            let (a, mut rx) = agent();
            a.display_passkey(dev(), 42, 3);
            let AgentEvent::Ask { request, .. } = rx.recv().await.unwrap() else {
                panic!()
            };
            assert_eq!(
                request.prompt,
                PairingPrompt::DisplayPasskey {
                    passkey: 42,
                    entered: 3
                }
            );
            assert_eq!(request.prompt.passkey_text().as_deref(), Some("000042"));
        }
        // Phase `cancel_and_release_reach_the_ui`.
        {
            let (a, mut rx) = agent();
            a.cancel();
            assert!(matches!(rx.recv().await, Some(AgentEvent::Cancelled)));
            a.release();
            assert!(matches!(rx.recv().await, Some(AgentEvent::Released)));
        }
        // Phase `unknown_devices_are_named_by_address`.
        // An unknown device still gets a nameable prompt — the address decoded from the path —
        // rather than an empty title.
        {
            let (a, mut rx) =
                PairingAgent::with_timeout(DeviceNames::new(), Duration::from_millis(50));
            a.display_passkey(dev(), 1, 0);
            let AgentEvent::Ask { request, .. } = rx.recv().await.unwrap() else {
                panic!()
            };
            assert_eq!(request.device_name, "AA:BB:CC:DD:EE:FF");
        }
    }

    /// Each `org.bluez.Agent1` method raises exactly its prompt, and the
    /// answer comes back in the shape that method returns.
    #[tokio::test]
    async fn every_agent1_method_maps_to_its_prompt() {
        const UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";
        let (a, mut rx) = agent();
        let ui = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..5 {
                let Some(AgentEvent::Ask { request, reply }) = rx.recv().await else {
                    panic!("every asking method raises a prompt")
                };
                let answer = match request.prompt {
                    PairingPrompt::RequestPinCode => PairingReply::PinCode("0000".into()),
                    PairingPrompt::RequestPasskey => PairingReply::Passkey(123_456),
                    _ => PairingReply::Accept,
                };
                seen.push(request.prompt);
                reply.send(answer).unwrap();
            }
            (seen, rx)
        });
        assert_eq!(a.request_pin_code(dev()).await.unwrap(), "0000");
        assert_eq!(a.request_passkey(dev()).await.unwrap(), 123_456);
        a.request_confirmation(dev(), 424_242).await.unwrap();
        a.request_authorization(dev()).await.unwrap();
        a.authorize_service(dev(), UUID.into()).await.unwrap();
        let (seen, mut rx) = ui.await.unwrap();
        assert_eq!(
            seen,
            vec![
                PairingPrompt::RequestPinCode,
                PairingPrompt::RequestPasskey,
                PairingPrompt::ConfirmPasskey { passkey: 424_242 },
                PairingPrompt::ConfirmPairing,
                PairingPrompt::AuthorizeService { uuid: UUID.into() },
            ]
        );

        // The display methods return at once, carrying the code to show.
        a.display_passkey(dev(), 42, 2);
        a.display_pin_code(dev(), "1234".into());
        for expected in [
            PairingPrompt::DisplayPasskey {
                passkey: 42,
                entered: 2,
            },
            PairingPrompt::DisplayPinCode { pin: "1234".into() },
        ] {
            let Some(AgentEvent::Ask { request, .. }) = rx.recv().await else {
                panic!("a display method notifies the UI")
            };
            assert_eq!(request.prompt, expected);
            assert!(!request.prompt.expects_answer());
        }

        // `Cancel` withdraws whatever is on screen.
        a.cancel();
        assert!(matches!(rx.recv().await, Some(AgentEvent::Cancelled)));
    }

    #[tokio::test]
    async fn unanswered_dismissed_or_mismatched_prompts_refuse_pairing() {
        // Phase `an_unanswered_prompt_times_out_as_canceled`.
        // Nobody answering must not wedge `bluetoothd` — it gets `Canceled`.
        {
            let (a, _rx) = agent();
            let err = a.request_confirmation(dev(), 1).await.unwrap_err();
            assert!(matches!(err, AgentError::Canceled(_)), "{err:?}");
        }
        // Phase `a_dismissed_prompt_is_a_refusal`.
        // A UI that closes the prompt without answering is a refusal, never a silent accept.
        {
            let (a, mut rx) = agent();
            tokio::spawn(async move {
                let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                    panic!()
                };
                drop(reply);
            });
            let err = a.request_authorization(dev()).await.unwrap_err();
            assert!(matches!(err, AgentError::Rejected(_)), "{err:?}");
        }
        // Phase `no_ui_attached_refuses_instead_of_accepting`.
        // With no UI attached at all the agent refuses rather than accepting — otherwise a
        // headless session would auto-pair anything nearby.
        {
            let (a, rx) = agent();
            drop(rx);
            let err = a.request_confirmation(dev(), 1).await.unwrap_err();
            assert!(matches!(err, AgentError::Rejected(_)), "{err:?}");
        }
        // Phase `a_mismatched_reply_cancels_rather_than_guessing`.
        // A UI that answers the wrong shape is a failed pairing, not a passkey of nonsense handed
        // to the daemon.
        {
            let (a, mut rx) = agent();
            tokio::spawn(async move {
                let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                    panic!()
                };
                reply.send(PairingReply::Accept).unwrap();
            });
            let err = a.request_passkey(dev()).await.unwrap_err();
            assert!(matches!(err, AgentError::Canceled(_)), "{err:?}");
        }
        // Phase `dropping_the_agent_rejects_retained_pairing_replies`.
        {
            let (a, _rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            let (ui_reply, reply_rx) = PairingReplySender::channel();
            a.insert_reply(0, ui_reply.clone()).unwrap();
            drop(a);
            assert_eq!(reply_rx.await.unwrap(), PairingReply::Reject);
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn the_bounded_event_queue_rejects_overflow_and_drains_on_shutdown() {
        // Phase `a_full_event_queue_rejects_authorization_instead_of_accumulating_work`.
        {
            let (a, _rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            // Display-only events do not grant consent, so they may occupy the sole queue slot.  A
            // request that would authorise a device is rejected rather than being buffered without
            // bound or silently dropped.
            a.display_passkey(dev(), 1, 0);
            let err = a.request_confirmation(dev(), 1).await.unwrap_err();
            assert!(
                matches!(err, AgentError::Rejected(message) if message == "pairing UI is busy")
            );
        }
        // Phase `request_id_exhaustion_never_wraps_or_replaces_an_active_reply`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 2);
            a.next_request_id.store(u64::MAX - 1, Ordering::Relaxed);
            let a = Arc::new(a);

            let first_request = {
                let a = Arc::clone(&a);
                tokio::spawn(async move { a.request_confirmation(dev(), 1).await })
            };
            let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                panic!("the final usable request ID should be admitted once");
            };
            assert!(
                a.active_replies
                    .lock()
                    .is_ok_and(|replies| replies.contains_key(&(u64::MAX - 1)))
            );

            let err = a.request_confirmation(dev(), 2).await.unwrap_err();
            assert!(
                matches!(err, AgentError::Rejected(message) if message == "pairing request ID space exhausted")
            );
            assert!(
                a.active_replies.lock().is_ok_and(
                    |replies| replies.len() == 1 && replies.contains_key(&(u64::MAX - 1))
                )
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(20), rx.recv())
                    .await
                    .is_err(),
                "an exhausted ID must not admit a second event"
            );

            reply.send(PairingReply::Reject).unwrap();
            assert!(matches!(
                first_request.await.unwrap(),
                Err(AgentError::Rejected(_))
            ));
        }
        // Phase `draining_a_bounded_queue_admits_the_next_pairing_request`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            let a = Arc::new(a);
            a.display_passkey(dev(), 1, 0);
            assert!(matches!(rx.recv().await, Some(AgentEvent::Ask { .. })));

            let asking = {
                let a = Arc::clone(&a);
                tokio::spawn(async move { a.request_confirmation(dev(), 2).await })
            };
            let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                panic!("expected the drained queue to admit an Ask");
            };
            reply.send(PairingReply::Accept).unwrap();
            assert!(asking.await.unwrap().is_ok());
        }
        // Phase `cancel_is_delivered_and_rejects_an_ask_even_when_the_queue_is_full`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            let a = Arc::new(a);
            let asking = {
                let a = Arc::clone(&a);
                tokio::spawn(async move { a.request_confirmation(dev(), 1).await })
            };
            tokio::task::yield_now().await;
            a.cancel();

            // Lifecycle is carried by a latest-state signal, rather than joining the already-full
            // prompt queue, so the compositor can always dismiss its current dialog.
            assert!(matches!(rx.recv().await, Some(AgentEvent::Cancelled)));
            let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                panic!("the queued Ask should still be drainable after cancellation");
            };
            assert_eq!(reply.send(PairingReply::Accept), Err(PairingReply::Accept));
            assert!(matches!(
                asking.await.unwrap(),
                Err(AgentError::Rejected(_))
            ));
        }
        // Phase `release_rejects_pending_work_and_prevents_new_requests`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            let a = Arc::new(a);
            let asking = {
                let a = Arc::clone(&a);
                tokio::spawn(async move { a.request_confirmation(dev(), 1).await })
            };
            tokio::task::yield_now().await;
            a.release();

            assert!(matches!(rx.recv().await, Some(AgentEvent::Released)));
            assert!(matches!(
                asking.await.unwrap(),
                Err(AgentError::Rejected(_))
            ));
            let err = a.request_confirmation(dev(), 2).await.unwrap_err();
            assert!(
                matches!(err, AgentError::Rejected(message) if message == "pairing agent released")
            );
        }
        // Phase `receiver_shutdown_drains_admitted_events_then_closes`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            a.display_passkey(dev(), 1, 0);
            rx.close();
            // Closing the receiver is a clean UI shutdown: already-admitted work is observable,
            // but no later D-Bus call can add more state.
            assert!(matches!(rx.recv().await, Some(AgentEvent::Ask { .. })));
            drop(a);
            assert!(rx.recv().await.is_none());
        }
        // Phase `oversized_daemon_strings_do_not_enter_the_event_queue`.
        {
            let (a, mut rx) =
                PairingAgent::with_event_capacity(DeviceNames::new(), Duration::from_secs(1), 1);
            a.display_pin_code(dev(), "x".repeat(MAX_PIN_CODE_BYTES + 1));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), rx.recv())
                    .await
                    .is_err()
            );

            let err = a
                .authorize_service(dev(), "x".repeat(MAX_SERVICE_UUID_BYTES + 1))
                .await
                .unwrap_err();
            assert!(matches!(err, AgentError::Canceled(_)));
        }
    }

    #[test]
    fn the_name_cache_is_bounded_and_falls_back_to_addresses() {
        // Phase `oversized_names_are_not_retained_in_the_cache`.
        {
            let names = DeviceNames::new();
            let path = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF";
            names.set(path, &"x".repeat(MAX_DEVICE_NAME_BYTES + 1));
            assert_eq!(names.get(path), "AA:BB:CC:DD:EE:FF");
        }
        // Phase `a_full_name_cache_uses_the_address_for_new_devices`.
        {
            let names = DeviceNames::with_capacity(1);
            names.set("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", "first");
            let second = "/org/bluez/hci0/dev_11_22_33_44_55_66";
            names.set(second, "second");
            assert_eq!(names.get(second), "11:22:33:44:55:66");
        }
    }
}
