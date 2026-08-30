//! The `org.bluez.Agent1` pairing agent.
//!
//! `BlueZ` does not *tell* a desktop how pairing is going — it **asks**: `bluetoothd` makes a
//! blocking D-Bus call into a registered object and waits for the pairing decision, so this
//! cannot be a `bluetoothctl` wrapper (there's no CLI shape for "the daemon calls you and
//! waits"). This object does no deciding itself — it translates each `BlueZ` question into a
//! [`PairingRequest`], sends it to whoever owns the screen, and turns the [`PairingReply`]
//! back into a return value or D-Bus error. The compositor draws the prompt; this is the wire.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use zbus::zvariant::ObjectPath;
use zbus::{Connection, DBusError, interface};

use crate::model::{PairingPrompt, PairingReply, PairingRequest, address_from_path, reply_answers};

/// Where the agent object lives on our own connection.
pub const AGENT_PATH: &str = "/org/qos/BluetoothAgent";

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
        reply: oneshot::Sender<PairingReply>,
    },
    /// `BlueZ` withdrew the pending request (the remote gave up, or pairing failed elsewhere).
    /// Any prompt on screen should be dismissed.
    Cancelled,
    /// `BlueZ` released the agent — it is no longer registered.
    Released,
}

/// The receiving end, held by whoever draws the prompt.
pub type AgentEvents = mpsc::UnboundedReceiver<AgentEvent>;

/// Display names for device paths, kept up to date by the client so the agent never has to
/// call back into `bluetoothd` while `bluetoothd` is waiting on it.
#[derive(Clone, Default)]
pub struct DeviceNames(Arc<Mutex<HashMap<String, String>>>);

impl DeviceNames {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, path: &str, name: &str) {
        if let Ok(mut m) = self.0.lock() {
            m.insert(path.to_string(), name.to_string());
        }
    }

    pub fn remove(&self, path: &str) {
        if let Ok(mut m) = self.0.lock() {
            m.remove(path);
        }
    }

    /// Best-effort display name: the cached alias, else the address decoded from the object
    /// path, else the path itself — never empty, so a prompt always has something to name.
    pub fn get(&self, path: &str) -> String {
        if let Ok(m) = self.0.lock()
            && let Some(n) = m.get(path).filter(|n| !n.is_empty())
        {
            return n.clone();
        }
        address_from_path(path).unwrap_or_else(|| path.to_string())
    }
}

/// The D-Bus object `BlueZ` calls into.
pub struct PairingAgent {
    tx: mpsc::UnboundedSender<AgentEvent>,
    names: DeviceNames,
    timeout: Duration,
}

impl PairingAgent {
    /// Build the agent and the receiver the UI drains.
    pub fn new(names: DeviceNames) -> (Self, AgentEvents) {
        Self::with_timeout(names, DEFAULT_ANSWER_TIMEOUT)
    }

    pub fn with_timeout(names: DeviceNames, timeout: Duration) -> (Self, AgentEvents) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx, names, timeout }, rx)
    }

    /// Push a question to the UI and wait for the answer. Every failure mode collapses to a
    /// D-Bus error rather than a hang: no listener, a dropped reply channel, a reply that
    /// doesn't answer the question, or nobody answering in time.
    async fn ask(
        &self,
        device: &ObjectPath<'_>,
        prompt: PairingPrompt,
    ) -> Result<PairingReply, AgentError> {
        let path = device.as_str().to_string();
        let request = PairingRequest {
            device_path: path.clone(),
            device_name: self.names.get(&path),
            prompt: prompt.clone(),
        };
        debug!(target: "qos::bluez", device = %path, prompt = ?prompt, "pairing prompt raised");
        let (reply_tx, reply_rx) = oneshot::channel();
        if self
            .tx
            .send(AgentEvent::Ask {
                request,
                reply: reply_tx,
            })
            .is_err()
        {
            // Nothing is listening. Refusing is the safe answer — silently accepting would pair a
            // device the user never saw.
            warn!(target: "qos::bluez", device = %path, "no pairing UI attached; refusing");
            return Err(AgentError::Rejected("no pairing UI attached".into()));
        }
        let reply = match tokio::time::timeout(self.timeout, reply_rx).await {
            Ok(Ok(reply)) => reply,
            Ok(Err(_)) => {
                warn!(target: "qos::bluez", device = %path, "pairing prompt dismissed without an answer");
                return Err(AgentError::Rejected("prompt dismissed".into()));
            }
            Err(_) => {
                warn!(target: "qos::bluez", device = %path, "pairing prompt timed out");
                return Err(AgentError::Canceled("no answer in time".into()));
            }
        };
        if !reply_answers(&prompt, &reply) {
            warn!(target: "qos::bluez", device = %path, ?reply, "reply does not answer the prompt");
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
        let path = device.as_str().to_string();
        let (reply_tx, _reply_rx) = oneshot::channel();
        let _ = self.tx.send(AgentEvent::Ask {
            request: PairingRequest {
                device_path: path.clone(),
                device_name: self.names.get(&path),
                prompt,
            },
            reply: reply_tx,
        });
    }
}

#[interface(name = "org.bluez.Agent1")]
impl PairingAgent {
    /// `BlueZ` is done with us.
    async fn release(&self) {
        debug!(target: "qos::bluez", "agent released by bluez");
        let _ = self.tx.send(AgentEvent::Released);
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
    async fn display_pin_code(&self, device: ObjectPath<'_>, pincode: String) {
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
    async fn display_passkey(&self, device: ObjectPath<'_>, passkey: u32, entered: u16) {
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
    async fn cancel(&self) {
        debug!(target: "qos::bluez", "bluez cancelled the pending pairing request");
        let _ = self.tx.send(AgentEvent::Cancelled);
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
    pub async fn register(connection: &Connection, agent: PairingAgent) -> zbus::Result<Self> {
        Self::register_at(connection, agent, AGENT_PATH, AGENT_CAPABILITY).await
    }

    /// Register at an explicit path and capability. Tests use this to run several agents on one bus.
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
            warn!(target: "qos::bluez", error = %e, "could not become the default pairing agent");
        }
        debug!(target: "qos::bluez", path, capability, "pairing agent registered");
        Ok(Self {
            connection: connection.clone(),
            path: path.to_string(),
        })
    }

    /// Unregister from `BlueZ` and drop the object.
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

    fn agent() -> (PairingAgent, AgentEvents) {
        let names = DeviceNames::new();
        names.set("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF", "Studio Headphones");
        PairingAgent::with_timeout(names, Duration::from_millis(200))
    }

    fn dev() -> ObjectPath<'static> {
        ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF").unwrap()
    }

    /// The prompt reaching the UI carries the passkey and the device's display name, and
    /// accepting returns `Ok` to `BlueZ`.
    #[tokio::test]
    async fn confirmation_prompt_reaches_the_ui_and_accepting_unblocks_bluez() {
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

    /// Declining returns `org.bluez.Error.Rejected`, which aborts the pairing rather than
    /// leaving it hanging.
    #[tokio::test]
    async fn declining_returns_rejected() {
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

    /// A typed passkey comes back to `BlueZ` as the u32 it asked for.
    #[tokio::test]
    async fn typed_passkey_is_returned_verbatim() {
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

    #[tokio::test]
    async fn typed_pin_is_returned_verbatim() {
        let (a, mut rx) = agent();
        tokio::spawn(async move {
            let AgentEvent::Ask { reply, .. } = rx.recv().await.unwrap() else {
                panic!()
            };
            reply.send(PairingReply::PinCode("0000".into())).unwrap();
        });
        assert_eq!(a.request_pin_code(dev()).await.unwrap(), "0000");
    }

    /// Nobody answering must not wedge `bluetoothd` — it gets `Canceled`.
    #[tokio::test]
    async fn an_unanswered_prompt_times_out_as_canceled() {
        let (a, _rx) = agent();
        let err = a.request_confirmation(dev(), 1).await.unwrap_err();
        assert!(matches!(err, AgentError::Canceled(_)), "{err:?}");
    }

    /// A UI that closes the prompt without answering is a refusal, never a silent accept.
    #[tokio::test]
    async fn a_dismissed_prompt_is_a_refusal() {
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

    /// With no UI attached at all the agent refuses rather than accepting — otherwise a
    /// headless session would auto-pair anything nearby.
    #[tokio::test]
    async fn no_ui_attached_refuses_instead_of_accepting() {
        let (a, rx) = agent();
        drop(rx);
        let err = a.request_confirmation(dev(), 1).await.unwrap_err();
        assert!(matches!(err, AgentError::Rejected(_)), "{err:?}");
    }

    /// A UI that answers the wrong shape is a failed pairing, not a passkey of nonsense handed
    /// to the daemon.
    #[tokio::test]
    async fn a_mismatched_reply_cancels_rather_than_guessing() {
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

    /// Display-only prompts return immediately and still tell the UI what to put on screen.
    #[tokio::test]
    async fn display_passkey_returns_at_once_and_still_notifies() {
        let (a, mut rx) = agent();
        a.display_passkey(dev(), 42, 3).await;
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

    #[tokio::test]
    async fn cancel_and_release_reach_the_ui() {
        let (a, mut rx) = agent();
        a.cancel().await;
        assert!(matches!(rx.recv().await, Some(AgentEvent::Cancelled)));
        a.release().await;
        assert!(matches!(rx.recv().await, Some(AgentEvent::Released)));
    }

    /// An unknown device still gets a nameable prompt — the address decoded from the path —
    /// rather than an empty title.
    #[tokio::test]
    async fn unknown_devices_are_named_by_address() {
        let (a, mut rx) = PairingAgent::with_timeout(DeviceNames::new(), Duration::from_millis(50));
        a.display_passkey(dev(), 1, 0).await;
        let AgentEvent::Ask { request, .. } = rx.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(request.device_name, "AA:BB:CC:DD:EE:FF");
    }
}
